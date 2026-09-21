//! Stable host transport with replaceable, versioned MCP workers. Daemon
//! management stays in the shared context coordinator; host requests are never
//! replayed when a worker or daemon connection is interrupted.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use nits_protocol::{
    BuildDescriptor, InstalledCandidate, ManagedDaemonState, ReleaseRelation, UpgradeFailure,
    UpgradeFailureKind, UpgradeIntent, UpgradeOperation, UpgradeProgress, UpgradeResult,
    UpgradeStage, WorkerVersion,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, BufReader, Lines};
use tokio::process::{Child, ChildStdout};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::checkpoint::SessionCheckpoint;
use crate::jsonrpc::{self, Incoming, Outgoing, RequestId};
use crate::management::{AdapterStatus, DaemonStatus, DaemonUpgrade};
use crate::server::{Endpoint, Method, ToolError, tool_reply};
use crate::tools::{ContextIdentity, ManagementCall, NoArgs, ToolBehavior, ToolContract, ToolName};
use crate::worker::{self, Ticket};

const WORKER_START_BUDGET: Duration = Duration::from_secs(8);
const WORKER_STOP_BUDGET: Duration = Duration::from_secs(2);
const UPGRADE_MONITOR_BUDGET: Duration = Duration::from_secs(90);

#[derive(Debug)]
struct Worker {
    process: WorkerProcess,
    build: BuildDescriptor,
    tools: HashMap<String, ToolBehavior>,
}

#[derive(Debug)]
enum WorkerProcess {
    Running {
        child: Child,
        input: mpsc::Sender<worker::Input>,
        writer: tokio::task::JoinHandle<anyhow::Result<()>>,
        output: Lines<BufReader<ChildStdout>>,
    },
    Unavailable {
        reason: String,
    },
}

impl Drop for WorkerProcess {
    fn drop(&mut self) {
        match self {
            Self::Running { writer, .. } => writer.abort(),
            Self::Unavailable { .. } => {}
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplacementPolicy {
    RetainSameBuild,
    RepairWorker,
}

impl Worker {
    async fn spawn(
        candidate: nitsd::build::InspectedBuild,
        checkpoint: SessionCheckpoint,
        cache: PathBuf,
    ) -> anyhow::Result<Self> {
        let frozen = candidate.clone();
        let path = tokio::task::spawn_blocking(move || frozen.freeze(&cache)).await??;
        let mut child = tokio::process::Command::new(path)
            // The worker executes pinned bytes, but future daemon management
            // must retain the selected installation path, including its alias.
            .env(nitsd::launch::NITS_BIN_ENV, &candidate.program)
            .arg("mcp-worker")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let mut input = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("worker stdin unavailable"))?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("worker stdout unavailable"))?;
        let mut output = BufReader::new(output).lines();
        let expected = checkpoint.clone();
        let ready = tokio::time::timeout(WORKER_START_BUDGET, async {
            worker::write(
                &mut input,
                &worker::Input::Restore {
                    version: WorkerVersion::CURRENT,
                    checkpoint,
                },
            )
            .await?;
            let line = output
                .next_line()
                .await?
                .ok_or_else(|| anyhow::anyhow!("worker exited before readiness"))?;
            Ok::<worker::Output, anyhow::Error>(serde_json::from_str(&line)?)
        })
        .await??;
        let worker::OutputEvent::Ready {
            version,
            build,
            tools,
        } = ready.event
        else {
            anyhow::bail!("worker answered before readiness");
        };
        anyhow::ensure!(
            version == WorkerVersion::CURRENT && build == candidate.descriptor,
            "worker readiness does not match the verified installed build"
        );
        anyhow::ensure!(
            ready.checkpoint == expected,
            "worker altered restored session during preflight"
        );
        let tools = validate_contract(tools)?;
        let (sender, mut queued) = mpsc::channel(64);
        let writer = tokio::spawn(async move {
            while let Some(message) = queued.recv().await {
                worker::write(&mut input, &message).await?;
            }
            Ok(())
        });
        Ok(Self {
            process: WorkerProcess::Running {
                child,
                input: sender,
                writer,
                output,
            },
            build,
            tools,
        })
    }

    fn send(&self, message: worker::Input) -> Result<(), String> {
        match &self.process {
            WorkerProcess::Running { input, .. } => input.try_send(message).map_err(|error|format!("request was not forwarded to the worker: {error}; retry after outstanding calls finish")),
            WorkerProcess::Unavailable { reason } => Err(format!("MCP worker unavailable: {reason}; this request was not forwarded. Use get_daemon_status or restart_daemon to recover.")),
        }
    }

    fn replacement_policy(&self) -> ReplacementPolicy {
        match self.process {
            WorkerProcess::Running { .. } => ReplacementPolicy::RetainSameBuild,
            WorkerProcess::Unavailable { .. } => ReplacementPolicy::RepairWorker,
        }
    }

    fn lost(&mut self, reason: String) {
        self.process = WorkerProcess::Unavailable { reason };
    }

    async fn next_line(&mut self) -> std::io::Result<Option<String>> {
        match &mut self.process {
            WorkerProcess::Running { output, .. } => output.next_line().await,
            WorkerProcess::Unavailable { .. } => std::future::pending().await,
        }
    }

    async fn stop(&mut self) {
        if let WorkerProcess::Running { child, input, .. } = &mut self.process {
            let finished = tokio::time::timeout(WORKER_STOP_BUDGET, async {
                input.send(worker::Input::Shutdown {}).await?;
                child.wait().await?;
                Ok::<(), anyhow::Error>(())
            })
            .await;
            if !matches!(finished, Ok(Ok(()))) {
                // Only the owned adapter child is stopped. Native drain owns
                // accepted daemon mutations independently of its connection.
                let _ = child.kill().await;
            }
        }
        self.lost("worker stopped".into());
    }
}

fn validate_contract(
    contracts: Vec<ToolContract>,
) -> anyhow::Result<HashMap<String, ToolBehavior>> {
    let mut tools = HashMap::new();
    for contract in contracts {
        anyhow::ensure!(
            !contract.name.is_empty(),
            "worker advertised an empty tool name"
        );
        match contract.behavior {
            ToolBehavior::Status => anyhow::ensure!(
                contract.name == ToolName::GetDaemonStatus.to_string(),
                "worker changed the stable status tool identity"
            ),
            ToolBehavior::Upgrade => anyhow::ensure!(
                contract.name == ToolName::RestartDaemon.to_string(),
                "worker changed the stable restart tool identity"
            ),
            ToolBehavior::Read
            | ToolBehavior::Mutation
            | ToolBehavior::Session
            | ToolBehavior::Wait => {}
        }
        anyhow::ensure!(
            tools.insert(contract.name, contract.behavior).is_none(),
            "worker advertised duplicate tool names"
        );
    }
    anyhow::ensure!(
        tools.get(&ToolName::GetDaemonStatus.to_string()) == Some(&ToolBehavior::Status)
            && tools.get(&ToolName::RestartDaemon.to_string()) == Some(&ToolBehavior::Upgrade),
        "worker omitted stable management capabilities"
    );
    Ok(tools)
}

fn context(endpoint: &Endpoint) -> ContextIdentity {
    ContextIdentity {
        name: endpoint.selection.name.clone(),
        kind: endpoint.selection.context.kind(),
    }
}

fn compatibility(
    adapter: &BuildDescriptor,
    daemon: &nits_protocol::ManagedDaemonStatus,
) -> AdapterStatus {
    match &daemon.running {
        ManagedDaemonState::Running { build } if build.protocol != adapter.protocol => {
            AdapterStatus::UpgradeRequired {
                reason: format!(
                    "running daemon requires protocol {}; this local MCP worker speaks {}. Install a compatible local Nits build and call restart_daemon again.",
                    build.protocol, adapter.protocol
                ),
            }
        }
        ManagedDaemonState::Running { .. }
        | ManagedDaemonState::Stopped {}
        | ManagedDaemonState::Legacy { .. }
        | ManagedDaemonState::Unavailable { .. }
        | ManagedDaemonState::NotManaged {} => AdapterStatus::Ready {},
    }
}

#[derive(Debug)]
enum ManagementEvent {
    Status {
        ticket: Ticket,
        result: Result<DaemonStatus, String>,
    },
    Accepted {
        operation: UpgradeOperation,
    },
    Finished {
        result: DaemonUpgrade,
        replacement: Replacement,
    },
}

#[derive(Debug)]
enum Replacement {
    Keep,
    Install(Worker),
}

#[derive(Debug)]
struct PreparedUpgrade {
    replacement: Replacement,
    activation: Activation,
}

#[derive(Debug)]
enum Activation {
    Unmanaged,
    Verified { daemon: BuildDescriptor },
}

impl Activation {
    async fn run(
        &self,
        endpoint: &Endpoint,
    ) -> Result<UpgradeResult, nitsd::contexts::ContextError> {
        match self {
            Self::Unmanaged => {
                nitsd::contexts::upgrade(&endpoint.selection.context, UpgradeIntent::Explicit).await
            }
            Self::Verified { daemon } => {
                nitsd::contexts::upgrade_verified(
                    &endpoint.selection.context,
                    UpgradeIntent::Explicit,
                    daemon.digest,
                )
                .await
            }
        }
    }

    fn checked(&self, result: UpgradeResult, stage: UpgradeStage) -> UpgradeResult {
        let Self::Verified { daemon: expected } = self else {
            return result;
        };
        let actual = match &result {
            UpgradeResult::AlreadyCurrent { build } => build,
            UpgradeResult::Accepted { operation } | UpgradeResult::Restarted { operation } => {
                &operation.target
            }
            UpgradeResult::Failed { .. } => return result,
        };
        if actual == expected {
            result
        } else {
            UpgradeResult::Failed {
                failure: UpgradeFailure {
                    stage,
                    kind: UpgradeFailureKind::ContextMismatch,
                    message: format!(
                        "daemon activation selected build {} instead of preflighted {}; the existing MCP worker was retained. Inspect get_daemon_status before retrying.",
                        actual.digest, expected.digest
                    ),
                },
            }
        }
    }
}

fn failed(
    endpoint: &Endpoint,
    kind: UpgradeFailureKind,
    stage: UpgradeStage,
    message: impl Into<String>,
) -> DaemonUpgrade {
    let message = message.into();
    DaemonUpgrade {
        context: context(endpoint),
        result: UpgradeResult::Failed {
            failure: UpgradeFailure {
                stage,
                kind,
                message: message.clone(),
            },
        },
        adapter: AdapterStatus::Retained { reason: message },
    }
}

async fn status(
    endpoint: Endpoint,
    supervisor_build: BuildDescriptor,
    adapter_build: BuildDescriptor,
    worker_state: AdapterStatus,
) -> Result<DaemonStatus, String> {
    let (daemon, installed_adapter) = tokio::join!(
        nitsd::contexts::upgrade_status(&endpoint.selection.context),
        crate::management::installed_adapter()
    );
    let daemon = daemon.map_err(|error| error.to_string())?;
    let adapter = match daemon
        .operation
        .as_ref()
        .map(|operation| (&operation.id, &operation.progress))
    {
        Some((operation, UpgradeProgress::Active { .. })) => AdapterStatus::Handoff {
            operation: *operation,
        },
        Some((_, UpgradeProgress::Ready {} | UpgradeProgress::Failed { .. })) | None => {
            match worker_state {
                AdapterStatus::Ready {} => compatibility(&adapter_build, &daemon),
                AdapterStatus::Preparing {}
                | AdapterStatus::Unavailable { .. }
                | AdapterStatus::Retained { .. }
                | AdapterStatus::UpgradeRequired { .. }
                | AdapterStatus::Handoff { .. } => worker_state,
            }
        }
    };
    Ok(DaemonStatus {
        context: context(&endpoint),
        supervisor_build,
        adapter_build,
        installed_adapter,
        daemon,
        adapter,
    })
}

async fn activate(
    checkpoint: SessionCheckpoint,
    current: BuildDescriptor,
    policy: ReplacementPolicy,
    cache: PathBuf,
    events: mpsc::UnboundedSender<ManagementEvent>,
) -> (DaemonUpgrade, Replacement) {
    let endpoint = checkpoint.endpoint().clone();
    let prepared = prepare_replacement(&checkpoint, &current, policy, cache).await;
    let PreparedUpgrade {
        replacement,
        activation,
    } = match prepared {
        Ok(prepared) => prepared,
        Err(message) => {
            return (
                failed(
                    &endpoint,
                    UpgradeFailureKind::CandidateUnavailable,
                    UpgradeStage::PreparingRestart,
                    message,
                ),
                Replacement::Keep,
            );
        }
    };
    let result = match activation.run(&endpoint).await {
        Ok(result) => result,
        Err(error) => {
            return (
                failed(
                    &endpoint,
                    UpgradeFailureKind::Io,
                    UpgradeStage::PreparingRestart,
                    error.to_string(),
                ),
                Replacement::Keep,
            );
        }
    };
    let result = activation.checked(result, UpgradeStage::PreparingRestart);
    let result = match result {
        UpgradeResult::Accepted { operation } => {
            let _ = events.send(ManagementEvent::Accepted {
                operation: operation.clone(),
            });
            monitor(&endpoint, operation).await
        }
        UpgradeResult::AlreadyCurrent { .. }
        | UpgradeResult::Restarted { .. }
        | UpgradeResult::Failed { .. } => result,
    };
    let result = activation.checked(result, UpgradeStage::ConfirmingReady);
    match &result {
        UpgradeResult::AlreadyCurrent { .. } | UpgradeResult::Restarted { .. } => (
            DaemonUpgrade {
                context: context(&endpoint),
                result,
                adapter: AdapterStatus::Ready {},
            },
            replacement,
        ),
        UpgradeResult::Failed { failure } => (
            DaemonUpgrade {
                context: context(&endpoint),
                adapter: AdapterStatus::Retained {
                    reason: failure.message.clone(),
                },
                result,
            },
            Replacement::Keep,
        ),
        UpgradeResult::Accepted { operation } => (
            DaemonUpgrade {
                context: context(&endpoint),
                adapter: AdapterStatus::Handoff {
                    operation: operation.id,
                },
                result,
            },
            Replacement::Keep,
        ),
    }
}

async fn prepare_replacement(
    checkpoint: &SessionCheckpoint,
    current: &BuildDescriptor,
    policy: ReplacementPolicy,
    cache: PathBuf,
) -> Result<PreparedUpgrade, String> {
    let endpoint = checkpoint.endpoint();
    // Management of raw WebSocket contexts remains the native coordinator's
    // explicit NotManaged outcome; there is no executable to guess.
    if matches!(endpoint.selection.context, nits_config::Context::Ws { .. }) {
        return Ok(PreparedUpgrade {
            replacement: Replacement::Keep,
            activation: Activation::Unmanaged,
        });
    }
    let candidate = nitsd::build::inspect(&nitsd::launch::nits_binary())
        .await
        .map_err(|error| error.to_string())?;
    match candidate.descriptor.release.relative_to(&current.release) {
        ReleaseRelation::Older | ReleaseRelation::DifferentChannel => return Err("the installed local MCP adapter is older or on another channel; refusing to downgrade this session".into()),
        ReleaseRelation::Equal | ReleaseRelation::Newer => {}
    }
    let daemon = nitsd::contexts::upgrade_status(&endpoint.selection.context)
        .await
        .map_err(|error| error.to_string())?;
    let installed = match daemon.installed {
        InstalledCandidate::Available { build } => build,
        InstalledCandidate::Unavailable { reason } => return Err(reason),
    };
    if installed.protocol != candidate.descriptor.protocol {
        return Err(format!(
            "installed daemon uses protocol {}, but the installed local MCP adapter uses {}; install a compatible local adapter before activation",
            installed.protocol, candidate.descriptor.protocol
        ));
    }
    let replacement = if candidate.descriptor.digest == current.digest
        && policy == ReplacementPolicy::RetainSameBuild
    {
        Replacement::Keep
    } else {
        Replacement::Install(
            Worker::spawn(candidate, checkpoint.clone(), cache)
                .await
                .map_err(|error| error.to_string())?,
        )
    };
    Ok(PreparedUpgrade {
        replacement,
        activation: Activation::Verified { daemon: installed },
    })
}

async fn monitor(endpoint: &Endpoint, mut operation: UpgradeOperation) -> UpgradeResult {
    let deadline = tokio::time::Instant::now() + UPGRADE_MONITOR_BUDGET;
    let mut stage = UpgradeStage::PreparingRestart;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return UpgradeResult::Failed { failure: UpgradeFailure { stage, kind: UpgradeFailureKind::ReadinessTimeout, message: "daemon activation exceeded the MCP monitoring budget; use get_daemon_status to inspect the durable operation before retrying".into() } };
        }
        match nitsd::contexts::upgrade_status(&endpoint.selection.context).await {
            Ok(status) => {
                if let Some(current) = status.operation
                    && current.id == operation.id
                {
                    operation = current;
                }
                match &operation.progress {
                    UpgradeProgress::Active { stage: current } => stage = *current,
                    UpgradeProgress::Ready {} => return UpgradeResult::Restarted { operation },
                    UpgradeProgress::Failed { failure } => {
                        return UpgradeResult::Failed {
                            failure: failure.clone(),
                        };
                    }
                }
            }
            Err(error) => {
                return UpgradeResult::Failed {
                    failure: UpgradeFailure {
                        stage,
                        kind: UpgradeFailureKind::Io,
                        message: error.to_string(),
                    },
                };
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

#[derive(Debug)]
enum Delivery {
    Reply(RequestId),
    Canceled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    Worker,
    Management,
}

#[derive(Debug)]
struct Pending {
    delivery: Delivery,
    behavior: ToolBehavior,
    owner: Owner,
}

#[derive(Debug)]
enum FlightStage {
    Preparing,
    Accepted { operation: UpgradeOperation },
}

#[derive(Debug)]
struct Flight {
    requesters: Vec<Ticket>,
    stage: FlightStage,
}

#[derive(Debug)]
struct Host {
    worker: Worker,
    checkpoint: SessionCheckpoint,
    supervisor_build: BuildDescriptor,
    cache: tempfile::TempDir,
    next_ticket: Ticket,
    pending: HashMap<Ticket, Pending>,
    external: HashMap<RequestId, Ticket>,
    queued: VecDeque<(Ticket, ManagementCall)>,
    flight: Option<Flight>,
    tasks: JoinSet<()>,
    events: mpsc::UnboundedSender<ManagementEvent>,
}

impl Host {
    fn ticket(&mut self) -> anyhow::Result<Ticket> {
        let ticket = self.next_ticket;
        self.next_ticket = ticket
            .next()
            .ok_or_else(|| anyhow::anyhow!("worker request identity exhausted"))?;
        Ok(ticket)
    }

    fn barriers(&self) -> bool {
        self.pending.values().any(|pending| {
            pending.owner == Owner::Worker && pending.behavior == ToolBehavior::Session
        })
    }

    async fn complete(
        &mut self,
        ticket: Ticket,
        reply: Option<Outgoing>,
        output: &mut (impl AsyncWrite + Unpin),
    ) -> anyhow::Result<()> {
        if let Some(pending) = self.pending.remove(&ticket)
            && let Delivery::Reply(id) = pending.delivery
        {
            if self.external.get(&id) == Some(&ticket) {
                self.external.remove(&id);
            }
            if let Some(mut reply) = reply {
                reply.id = id.value();
                worker::write(output, &reply).await?;
            }
        }
        Ok(())
    }

    async fn tool_error(
        &mut self,
        ticket: Ticket,
        message: impl Into<String>,
        output: &mut (impl AsyncWrite + Unpin),
    ) -> anyhow::Result<()> {
        let reply = tool_reply(Value::Null, Err(ToolError::Invalid(message.into())));
        self.complete(ticket, Some(reply), output).await
    }

    async fn tool_result(
        &mut self,
        ticket: Ticket,
        value: &impl serde::Serialize,
        output: &mut (impl AsyncWrite + Unpin),
    ) -> anyhow::Result<()> {
        let reply = tool_reply(Value::Null, Ok(serde_json::to_value(value)?));
        self.complete(ticket, Some(reply), output).await
    }

    async fn start_queued(&mut self, output: &mut (impl AsyncWrite + Unpin)) -> anyhow::Result<()> {
        if self.barriers() {
            return Ok(());
        }
        while let Some((ticket, call)) = self.queued.pop_front() {
            if !self.pending.contains_key(&ticket) {
                continue;
            }
            if !self.checkpoint.initialized() {
                self.tool_error(ticket, "call initialize before daemon management", output)
                    .await?;
                continue;
            }
            match call {
                ManagementCall::Status => {
                    let endpoint = self.checkpoint.endpoint().clone();
                    let supervisor = self.supervisor_build.clone();
                    let adapter = self.worker.build.clone();
                    let worker_state = match &self.flight {
                        Some(Flight {
                            stage: FlightStage::Preparing,
                            ..
                        }) => AdapterStatus::Preparing {},
                        Some(Flight {
                            stage: FlightStage::Accepted { operation },
                            ..
                        }) => AdapterStatus::Handoff {
                            operation: operation.id,
                        },
                        None => match &self.worker.process {
                            WorkerProcess::Running { .. } => AdapterStatus::Ready {},
                            WorkerProcess::Unavailable { reason } => AdapterStatus::Unavailable {
                                reason: reason.clone(),
                            },
                        },
                    };
                    let events = self.events.clone();
                    self.tasks.spawn(async move {
                        let result = status(endpoint, supervisor, adapter, worker_state).await;
                        let _ = events.send(ManagementEvent::Status { ticket, result });
                    });
                }
                ManagementCall::Restart => match &mut self.flight {
                    Some(Flight {
                        stage: FlightStage::Preparing,
                        requesters,
                    }) => requesters.push(ticket),
                    Some(Flight {
                        stage: FlightStage::Accepted { operation },
                        ..
                    }) => {
                        let value = DaemonUpgrade {
                            context: context(self.checkpoint.endpoint()),
                            result: UpgradeResult::Accepted {
                                operation: operation.clone(),
                            },
                            adapter: AdapterStatus::Handoff {
                                operation: operation.id,
                            },
                        };
                        self.tool_result(ticket, &value, output).await?;
                    }
                    None => {
                        self.flight = Some(Flight {
                            requesters: vec![ticket],
                            stage: FlightStage::Preparing,
                        });
                        let checkpoint = self.checkpoint.clone();
                        let current = self.worker.build.clone();
                        let policy = self.worker.replacement_policy();
                        let cache = self.cache.path().to_owned();
                        let events = self.events.clone();
                        self.tasks.spawn(async move {
                            let (result, replacement) =
                                activate(checkpoint, current, policy, cache, events.clone()).await;
                            let _ = events.send(ManagementEvent::Finished {
                                result,
                                replacement,
                            });
                        });
                    }
                },
            }
        }
        Ok(())
    }

    async fn worker_message(
        &mut self,
        message: worker::Output,
        output: &mut (impl AsyncWrite + Unpin),
    ) -> anyhow::Result<()> {
        match message.event {
            worker::OutputEvent::Ready { .. } => anyhow::bail!("worker sent duplicate readiness"),
            worker::OutputEvent::Handled { ticket, reply } => {
                self.checkpoint = message.checkpoint;
                self.complete(ticket, reply, output).await?;
            }
            worker::OutputEvent::Canceled { ticket } => {
                self.checkpoint = message.checkpoint;
                self.complete(ticket, None, output).await?;
            }
        }
        self.start_queued(output).await
    }

    async fn management_message(
        &mut self,
        message: ManagementEvent,
        output: &mut (impl AsyncWrite + Unpin),
    ) -> anyhow::Result<()> {
        match message {
            ManagementEvent::Status { ticket, result } => match result {
                Ok(value) => self.tool_result(ticket, &value, output).await?,
                Err(error) => self.tool_error(ticket, error, output).await?,
            },
            ManagementEvent::Accepted { operation } => {
                if let Some(flight) = &mut self.flight {
                    flight.stage = FlightStage::Accepted {
                        operation: operation.clone(),
                    };
                    let requesters = std::mem::take(&mut flight.requesters);
                    let value = DaemonUpgrade {
                        context: context(self.checkpoint.endpoint()),
                        result: UpgradeResult::Accepted {
                            operation: operation.clone(),
                        },
                        adapter: AdapterStatus::Handoff {
                            operation: operation.id,
                        },
                    };
                    for ticket in requesters {
                        self.tool_result(ticket, &value, output).await?;
                    }
                }
            }
            ManagementEvent::Finished {
                result,
                replacement,
            } => {
                if let Replacement::Install(replacement) = replacement {
                    let mut old = std::mem::replace(&mut self.worker, replacement);
                    self.interrupt_worker_requests(output).await?;
                    // Drain/stop the adapter outside the host loop. It has no
                    // remaining authority to complete a host request.
                    self.tasks.spawn(async move {
                        old.stop().await;
                    });
                    worker::write(
                        output,
                        &json!({"jsonrpc":"2.0", "method":"notifications/tools/list_changed"}),
                    )
                    .await?;
                }
                if let Some(flight) = self.flight.take() {
                    for ticket in flight.requesters {
                        self.tool_result(ticket, &result, output).await?;
                    }
                }
            }
        }
        self.start_queued(output).await
    }

    async fn interrupt_worker_requests(
        &mut self,
        output: &mut (impl AsyncWrite + Unpin),
    ) -> anyhow::Result<()> {
        let requests: Vec<_> = self
            .pending
            .iter()
            .filter_map(|(ticket, pending)| {
                (pending.owner == Owner::Worker).then_some((*ticket, pending.behavior))
            })
            .collect();
        for (ticket, behavior) in requests {
            let message = match behavior {
                ToolBehavior::Mutation => {
                    "worker replaced during this mutation; it may have committed. The request was not replayed. Inspect daemon state before repeating it."
                }
                ToolBehavior::Read | ToolBehavior::Wait | ToolBehavior::Session => {
                    "request interrupted by MCP worker replacement; issue a fresh read or resume from your last acknowledged context-scoped cursor"
                }
                ToolBehavior::Status | ToolBehavior::Upgrade => {
                    "management request interrupted by worker replacement"
                }
            };
            self.tool_error(ticket, message, output).await?;
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct Invocation {
    name: String,
    #[serde(default = "empty_arguments")]
    arguments: Value,
}

fn empty_arguments() -> Value {
    json!({})
}

#[derive(Debug, Clone, Copy)]
enum Route {
    Ping,
    Worker(ToolBehavior),
    Management(ManagementCall),
}

fn route(
    message: &Incoming,
    tools: &HashMap<String, ToolBehavior>,
) -> Result<Route, Box<Outgoing>> {
    let id = message.id.clone().unwrap_or(Value::Null);
    match message.method.parse::<Method>() {
        Ok(Method::Ping) => Ok(Route::Ping),
        Ok(Method::Initialize) => Ok(Route::Worker(ToolBehavior::Session)),
        Ok(Method::ToolsList | Method::Cancelled) | Err(_) => Ok(Route::Worker(ToolBehavior::Read)),
        Ok(Method::ToolsCall) => {
            let invocation: Invocation =
                serde_json::from_value(message.params.clone()).map_err(|error| {
                    Outgoing::error(
                        id.clone(),
                        jsonrpc::INVALID_PARAMS,
                        format!("invalid tools/call params: {error}"),
                    )
                })?;
            let behavior = tools
                .get(&invocation.name)
                .copied()
                .unwrap_or(ToolBehavior::Mutation);
            match behavior {
                ToolBehavior::Status => {
                    serde_json::from_value::<NoArgs>(invocation.arguments)
                        .map_err(|error| tool_reply(id, Err(ToolError::from(error))))?;
                    Ok(Route::Management(ManagementCall::Status))
                }
                ToolBehavior::Upgrade => {
                    serde_json::from_value::<NoArgs>(invocation.arguments)
                        .map_err(|error| tool_reply(id, Err(ToolError::from(error))))?;
                    Ok(Route::Management(ManagementCall::Restart))
                }
                ToolBehavior::Read
                | ToolBehavior::Mutation
                | ToolBehavior::Session
                | ToolBehavior::Wait => Ok(Route::Worker(behavior)),
            }
        }
    }
}

impl Host {
    fn cancel(&mut self, parameters: Value) {
        if let Ok(cancellation) = serde_json::from_value::<crate::server::Cancellation>(parameters)
            && let Some(ticket) = self.external.remove(&cancellation.request_id)
            && let Some(pending) = self.pending.get_mut(&ticket)
        {
            pending.delivery = Delivery::Canceled;
            match pending.owner {
                Owner::Worker => {
                    let _ = self.worker.send(worker::Input::Cancel { ticket });
                }
                Owner::Management => {
                    if let Some(position) =
                        self.queued.iter().position(|(queued, _)| *queued == ticket)
                    {
                        self.queued.remove(position);
                        self.pending.remove(&ticket);
                    }
                }
            }
        }
    }

    async fn incoming(
        &mut self,
        message: Incoming,
        output: &mut (impl AsyncWrite + Unpin),
    ) -> anyhow::Result<()> {
        if message.jsonrpc != "2.0" {
            worker::write(
                output,
                &Outgoing::error(
                    message.id.unwrap_or(Value::Null),
                    jsonrpc::INVALID_REQUEST,
                    "jsonrpc must be \"2.0\"",
                ),
            )
            .await?;
            return Ok(());
        }
        let Some(raw_id) = &message.id else {
            if matches!(message.method.parse::<Method>(), Ok(Method::Cancelled)) {
                self.cancel(message.params);
            }
            return Ok(());
        };
        let Ok(id) = serde_json::from_value::<RequestId>(raw_id.clone()) else {
            worker::write(
                output,
                &Outgoing::error(
                    Value::Null,
                    jsonrpc::INVALID_REQUEST,
                    "request id must be a string or number",
                ),
            )
            .await?;
            return Ok(());
        };
        if self.external.contains_key(&id) {
            worker::write(
                output,
                &Outgoing::error(
                    id.value(),
                    jsonrpc::INVALID_REQUEST,
                    "request id already has an outstanding call",
                ),
            )
            .await?;
            return Ok(());
        }
        let route = match route(&message, &self.worker.tools) {
            Ok(route) => route,
            Err(reply) => {
                worker::write(output, &reply).await?;
                return Ok(());
            }
        };
        if matches!(route, Route::Ping) {
            worker::write(output, &Outgoing::result(id.value(), json!({}))).await?;
            return Ok(());
        }
        if self.pending.len() >= 256 {
            worker::write(
                output,
                &tool_reply(
                    id.value(),
                    Err(ToolError::Invalid(
                        "at most 256 requests may be outstanding; this request was not forwarded"
                            .into(),
                    )),
                ),
            )
            .await?;
            return Ok(());
        }
        let ticket = self.ticket()?;
        let (behavior, owner) = match route {
            Route::Worker(behavior) => (behavior, Owner::Worker),
            Route::Management(ManagementCall::Status) => (ToolBehavior::Status, Owner::Management),
            Route::Management(ManagementCall::Restart) => {
                (ToolBehavior::Upgrade, Owner::Management)
            }
            Route::Ping => (ToolBehavior::Read, Owner::Management),
        };
        self.external.insert(id.clone(), ticket);
        self.pending.insert(
            ticket,
            Pending {
                delivery: Delivery::Reply(id),
                behavior,
                owner,
            },
        );
        self.dispatch_route(ticket, route, message, output).await
    }

    async fn dispatch_route(
        &mut self,
        ticket: Ticket,
        route: Route,
        message: Incoming,
        output: &mut (impl AsyncWrite + Unpin),
    ) -> anyhow::Result<()> {
        match route {
            Route::Ping => {}
            Route::Worker(behavior) => {
                let waiting_upgrade = self
                    .queued
                    .iter()
                    .any(|(_, call)| *call == ManagementCall::Restart);
                let waiting_session_boundary =
                    behavior == ToolBehavior::Session && !self.queued.is_empty();
                if self.flight.is_some() || waiting_upgrade || waiting_session_boundary {
                    self.tool_error(ticket, "daemon/MCP upgrade is in progress; this request was not forwarded. Use get_daemon_status, then retry when ready.", output).await?;
                } else if let Err(error) = self
                    .worker
                    .send(worker::Input::Dispatch { ticket, message })
                {
                    self.tool_error(ticket, error, output).await?;
                }
            }
            Route::Management(call) => {
                self.queued.push_back((ticket, call));
                self.start_queued(output).await?;
            }
        }
        Ok(())
    }
}

/// The host process retains initialized MCP transport state while a verified
/// installed implementation handles daemon-specific calls in a child process.
pub(crate) async fn serve(
    checkpoint: SessionCheckpoint,
    input: impl AsyncBufRead + Unpin,
    mut output: impl AsyncWrite + Unpin,
) -> anyhow::Result<()> {
    let cache = tempfile::tempdir()?;
    let supervisor_build = nitsd::build::running()?;
    let candidate = nitsd::build::inspect(&nitsd::launch::nits_binary()).await?;
    let worker = Worker::spawn(candidate, checkpoint.clone(), cache.path().to_owned()).await?;
    let (sender, mut events) = mpsc::unbounded_channel();
    let mut host = Host {
        worker,
        checkpoint,
        supervisor_build,
        cache,
        next_ticket: Ticket::FIRST,
        pending: HashMap::new(),
        external: HashMap::new(),
        queued: VecDeque::new(),
        flight: None,
        tasks: JoinSet::new(),
        events: sender,
    };
    let mut lines = input.lines();
    let result = async {
        loop {
            tokio::select! {
                line = lines.next_line() => {
                    let Some(line) = line? else { break; };
                    if line.trim().is_empty() { continue; }
                    match serde_json::from_str::<Incoming>(&line) {
                        Ok(message) => host.incoming(message, &mut output).await?,
                        Err(error) => worker::write(&mut output, &Outgoing::error(Value::Null, jsonrpc::PARSE_ERROR, format!("parse error: {error}"))).await?,
                    }
                }
                line = host.worker.next_line() => {
                    if let Ok(Some(line)) = line {
                        match serde_json::from_str(&line) {
                            Ok(message) => host.worker_message(message, &mut output).await?,
                            Err(error) => {
                                host.worker.lost(format!("invalid worker protocol: {error}"));
                                host.interrupt_worker_requests(&mut output).await?;
                                host.start_queued(&mut output).await?;
                            }
                        }
                    } else {
                        host.worker.lost("worker connection ended".into());
                        host.interrupt_worker_requests(&mut output).await?;
                        host.start_queued(&mut output).await?;
                    }
                }
                Some(event) = events.recv() => host.management_message(event, &mut output).await?,
                completed = host.tasks.join_next(), if !host.tasks.is_empty() => {
                    if let Some(Err(error)) = completed && !error.is_cancelled() { return Err(error.into()); }
                }
            }
        }
        Ok(())
    }.await;
    host.tasks.shutdown().await;
    host.worker.stop().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(id: &str, method: &str, params: Value) -> Incoming {
        Incoming {
            jsonrpc: "2.0".into(),
            id: Some(json!(id)),
            method: method.into(),
            params,
        }
    }

    fn host() -> Host {
        let cache = tempfile::tempdir().unwrap();
        let build = nitsd::build::running().unwrap();
        let server = crate::Server::new(
            Endpoint {
                selection: nits_config::Selection {
                    name: "fixture".parse().unwrap(),
                    context: nits_config::Context::Local {
                        data_dir: Some(cache.path().join("data")),
                        socket: Some(cache.path().join("absent.sock")),
                    },
                    origin: nits_config::SelectionOrigin::Flag,
                },
                config_path: cache.path().join("absent.toml"),
                start: nitsd::contexts::StartPolicy::RequireRunning,
            },
            crate::server::AgentIdentity {
                model: "model".into(),
                session_id: "session".into(),
                invoked_by: None,
            },
            nits_protocol::BuildInfo {
                name: "fixture".into(),
                version: "1".into(),
            },
        );
        let (events, _) = mpsc::unbounded_channel();
        Host {
            worker: Worker {
                build: build.clone(),
                tools: validate_contract(crate::tools::contracts()).unwrap(),
                process: WorkerProcess::Unavailable {
                    reason: "test worker has ended".into(),
                },
            },
            checkpoint: server.checkpoint(),
            supervisor_build: build,
            cache,
            next_ticket: Ticket::FIRST,
            pending: HashMap::new(),
            external: HashMap::new(),
            queued: VecDeque::new(),
            flight: None,
            tasks: JoinSet::new(),
            events,
        }
    }

    fn pending(host: &mut Host, id: &str, behavior: ToolBehavior) -> Ticket {
        let ticket = host.ticket().unwrap();
        let id = RequestId::String(id.into());
        host.external.insert(id.clone(), ticket);
        host.pending.insert(
            ticket,
            Pending {
                delivery: Delivery::Reply(id),
                behavior,
                owner: Owner::Worker,
            },
        );
        ticket
    }

    async fn next(output: &mut Lines<BufReader<tokio::io::DuplexStream>>) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(1), output.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        serde_json::from_str(&line).unwrap()
    }

    #[test]
    fn activation_requires_the_preflighted_descriptor_for_every_success_stage() {
        let expected = nitsd::build::running().unwrap();
        let activation = Activation::Verified {
            daemon: expected.clone(),
        };
        let mut changed = expected.clone();
        changed.digest = nits_protocol::BuildDigest::from_bytes([42; 32]);
        for build in [expected.clone(), changed] {
            let operation = UpgradeOperation {
                id: nits_protocol::UpgradeId::from_parts(1, 2),
                source: expected.clone(),
                target: build.clone(),
                progress: UpgradeProgress::Active {
                    stage: UpgradeStage::PreparingRestart,
                },
            };
            for result in [
                UpgradeResult::AlreadyCurrent {
                    build: build.clone(),
                },
                UpgradeResult::Accepted {
                    operation: operation.clone(),
                },
                UpgradeResult::Restarted { operation },
            ] {
                let checked = activation.checked(result.clone(), UpgradeStage::ConfirmingReady);
                if build == expected {
                    assert_eq!(checked, result);
                } else {
                    let UpgradeResult::Failed { failure } = checked else {
                        panic!("changed build must not complete a verified activation")
                    };
                    assert_eq!(failure.kind, UpgradeFailureKind::ContextMismatch);
                    assert_eq!(failure.stage, UpgradeStage::ConfirmingReady);
                    assert!(failure.message.contains("existing MCP worker was retained"));
                }
            }
        }
        let failure = UpgradeResult::Failed {
            failure: UpgradeFailure {
                stage: UpgradeStage::Draining,
                kind: UpgradeFailureKind::DrainTimeout,
                message: "owned work remains".into(),
            },
        };
        assert_eq!(
            activation.checked(failure.clone(), UpgradeStage::ConfirmingReady),
            failure
        );
        assert_eq!(
            Activation::Unmanaged.checked(failure.clone(), UpgradeStage::PreparingRestart),
            failure
        );
    }

    #[test]
    fn worker_contract_routes_future_names_and_rejects_management_substitution() {
        let mut contracts = crate::tools::contracts();
        contracts.push(ToolContract {
            name: "future_review_tool".into(),
            behavior: ToolBehavior::Read,
        });
        let tools = validate_contract(contracts).unwrap();
        assert!(matches!(
            route(
                &message(
                    "one",
                    "tools/call",
                    json!({"name":"future_review_tool", "arguments":{}})
                ),
                &tools
            )
            .unwrap(),
            Route::Worker(ToolBehavior::Read)
        ));
        let mut missing = crate::tools::contracts();
        missing.retain(|tool| tool.behavior != ToolBehavior::Upgrade);
        assert!(validate_contract(missing).is_err());
        let mut substituted = crate::tools::contracts();
        substituted.push(ToolContract {
            name: "arbitrary_shell".into(),
            behavior: ToolBehavior::Upgrade,
        });
        assert!(validate_contract(substituted).is_err());
        let rejected = route(
            &message(
                "one",
                "tools/call",
                json!({"name":"restart_daemon", "arguments":{"program":"/tmp/arbitrary"}}),
            ),
            &tools,
        )
        .unwrap_err();
        assert_eq!(rejected.result.unwrap()["isError"], true);
    }

    #[tokio::test]
    async fn canceled_host_id_reuse_cannot_receive_an_old_worker_reply() {
        let mut host = host();
        let ticket = pending(&mut host, "reused", ToolBehavior::Wait);
        let (mut output, reader) = tokio::io::duplex(16 * 1024);
        let mut reader = BufReader::new(reader).lines();
        host.incoming(
            Incoming {
                jsonrpc: "2.0".into(),
                id: None,
                method: "notifications/cancelled".into(),
                params: json!({"requestId":"reused"}),
            },
            &mut output,
        )
        .await
        .unwrap();
        host.incoming(message("reused", "ping", json!({})), &mut output)
            .await
            .unwrap();
        assert_eq!(
            next(&mut reader).await,
            json!({"jsonrpc":"2.0", "id":"reused", "result":{}})
        );
        host.worker_message(
            worker::Output {
                checkpoint: host.checkpoint.clone(),
                event: worker::OutputEvent::Handled {
                    ticket,
                    reply: Some(Outgoing::result(
                        ticket.request_id().value(),
                        json!({"old":true}),
                    )),
                },
            },
            &mut output,
        )
        .await
        .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(30), reader.next_line())
                .await
                .is_err()
        );
        assert!(host.pending.is_empty());
        assert!(host.external.is_empty());
    }

    #[tokio::test]
    async fn queued_restart_waits_for_preceding_session_change_and_cannot_be_overtaken() {
        let mut host = host();
        pending(&mut host, "prior-context", ToolBehavior::Session);
        let (mut output, reader) = tokio::io::duplex(16 * 1024);
        let mut reader = BufReader::new(reader).lines();
        host.incoming(
            message(
                "restart",
                "tools/call",
                json!({"name":"restart_daemon", "arguments":{}}),
            ),
            &mut output,
        )
        .await
        .unwrap();
        assert!(host.flight.is_none());
        assert_eq!(host.queued.len(), 1);
        host.incoming(
            message(
                "later-context",
                "tools/call",
                json!({"name":"use_context", "arguments":{"name":"wrong-target"}}),
            ),
            &mut output,
        )
        .await
        .unwrap();
        let rejected = next(&mut reader).await;
        assert_eq!(rejected["id"], "later-context");
        assert_eq!(rejected["result"]["isError"], true);
        assert!(
            rejected["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("not forwarded")
        );
        host.incoming(
            Incoming {
                jsonrpc: "2.0".into(),
                id: None,
                method: "notifications/cancelled".into(),
                params: json!({"requestId":"restart"}),
            },
            &mut output,
        )
        .await
        .unwrap();
        assert!(
            host.queued.is_empty(),
            "canceling a queued restart must not activate it later"
        );
        assert!(host.tasks.is_empty());
        assert!(host.flight.is_none());
    }

    #[tokio::test]
    async fn worker_loss_completes_requests_with_uncertainty_and_leaves_ping_usable() {
        let mut host = host();
        pending(&mut host, "mutation", ToolBehavior::Mutation);
        pending(&mut host, "read", ToolBehavior::Read);
        let (mut output, reader) = tokio::io::duplex(16 * 1024);
        let mut reader = BufReader::new(reader).lines();
        host.interrupt_worker_requests(&mut output).await.unwrap();
        let replies = [next(&mut reader).await, next(&mut reader).await];
        let mutation = replies
            .iter()
            .find(|reply| reply["id"] == "mutation")
            .unwrap();
        assert!(
            mutation["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("may have committed")
        );
        assert!(host.pending.is_empty());
        host.incoming(message("ping", "ping", json!({})), &mut output)
            .await
            .unwrap();
        assert_eq!(next(&mut reader).await["result"], json!({}));
        host.incoming(
            message(
                "read-again",
                "tools/call",
                json!({"name":"list_workspaces", "arguments":{}}),
            ),
            &mut output,
        )
        .await
        .unwrap();
        let rejected = next(&mut reader).await;
        assert!(
            rejected["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("not forwarded")
        );
        assert_eq!(
            host.worker.replacement_policy(),
            ReplacementPolicy::RepairWorker
        );
    }
}
