//! Installed-build handoff shared by every managed client adapter.
//!
//! A detached coordinator owns a stable per-store file lock. Requester
//! cancellation does not cancel accepted work or start a second replacement.
//! The journal records readiness only after ownership release and a matching
//! replacement control inspection plus application handshake.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use nits_protocol::{
    BuildDescriptor, InstalledCandidate, ManagedDaemonState, ManagedDaemonStatus, ReleaseRelation,
    UpgradeFailure, UpgradeFailureKind, UpgradeId, UpgradeIntent, UpgradeOperation,
    UpgradeProgress, UpgradeResult, UpgradeStage,
};
use serde::{Deserialize, Serialize};

use crate::control;
use crate::launch::{self, DaemonSpec};
use crate::ownership::{self, Ownership};

const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
const READY_TIMEOUT: Duration = Duration::from_secs(15);
const CALL_BUDGET: Duration = Duration::from_secs(8);
const POLL: Duration = Duration::from_millis(30);
const LOCK: &str = "upgrade.lock";
const JOURNAL: &str = "upgrade.json";
const ACTIVE_PLAN: &str = "upgrade-plan.json";
pub const START_OPERATION_ENV: &str = "NITS_UPGRADE_OPERATION";

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Plan {
    operation: UpgradeOperation,
    /// The last admitted operation observed before this request was spawned.
    /// A queued child cannot retry after an intervening admission, even when
    /// that operation failed before changing the incumbent.
    preflight_operation: Option<UpgradeId>,
    runtime: crate::serve::ServeOpts,
    program: PathBuf,
    installed: PathBuf,
    intent: UpgradeIntent,
}

/// Only a plan published after revalidation may finish the shared journal.
#[derive(Debug)]
struct ActivePlan(Plan);

#[derive(Debug)]
enum Revalidated {
    Activate,
    Complete(RequestOutcome),
}

/// A contender's terminal receipt is independent of the currently published
/// operation. Rejection before admission must not overwrite another owner's
/// progress, and callers must still receive their own precise failure.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum RequestOutcome {
    AlreadyCurrent { build: BuildDescriptor },
    Restarted { operation: UpgradeOperation },
    Failed { failure: UpgradeFailure },
}

impl From<RequestOutcome> for UpgradeResult {
    fn from(outcome: RequestOutcome) -> Self {
        match outcome {
            RequestOutcome::AlreadyCurrent { build } => Self::AlreadyCurrent { build },
            RequestOutcome::Restarted { operation } => Self::Restarted { operation },
            RequestOutcome::Failed { failure } => Self::Failed { failure },
        }
    }
}

fn outcome_path(data_dir: &Path, request: UpgradeId) -> PathBuf {
    data_dir.join(format!("upgrade-result-{request}.json"))
}

fn failed(
    stage: UpgradeStage,
    kind: UpgradeFailureKind,
    message: impl Into<String>,
) -> UpgradeFailure {
    UpgradeFailure {
        stage,
        kind,
        message: message.into(),
    }
}

fn io_failure(stage: UpgradeStage, error: &io::Error) -> UpgradeFailure {
    failed(stage, UpgradeFailureKind::Io, error.to_string())
}

/// Probes never create or replace the stable inode.
pub fn coordinator_active(data_dir: &Path) -> io::Result<bool> {
    let file = match File::open(data_dir.join(LOCK)) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    match file.try_lock() {
        Ok(()) => Ok(false),
        Err(TryLockError::WouldBlock) => Ok(true),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

/// The selected endpoint's existing association survives listener retirement.
/// Its configured startup default may name an entirely unrelated store.
pub fn endpoint_active(socket: &Path) -> io::Result<bool> {
    let Some(data_dir) = ownership::associated_data_dir(socket)? else {
        return Ok(false);
    };
    if !coordinator_active(&data_dir)? {
        return Ok(false);
    }
    let plan: Plan = match read_json(&data_dir.join(ACTIVE_PLAN)) {
        Ok(plan) => plan,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error),
    };
    same_endpoint(socket, &plan.runtime.socket)
}

async fn lock(data_dir: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(data_dir.join(LOCK))?;
    // Probe contention is transient, while another coordinator has a bounded
    // drain/readiness budget. Cancellation drops this waiter without retaining
    // a blocking thread or changing the incumbent's journal.
    let deadline = tokio::time::Instant::now() + DRAIN_TIMEOUT + READY_TIMEOUT + CALL_BUDGET;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::Error(error)) => return Err(error),
            Err(TryLockError::WouldBlock) => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "another upgrade owner still holds this store; inspect its recorded operation",
            ));
        }
        tokio::time::sleep(POLL).await;
    }
}

fn write_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}.{}", std::process::id(), fastrand::u64(..)));
    let temporary = PathBuf::from(temporary);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        serde_json::to_writer(&mut file, value).map_err(io::Error::other)?;
        file.flush()?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<T> {
    use std::io::Read;
    let mut bytes = Vec::new();
    File::open(path)?
        .take(128 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 128 * 1024 {
        return Err(io::Error::other("lifecycle journal exceeds size limit"));
    }
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

pub fn operation(data_dir: &Path) -> io::Result<Option<UpgradeOperation>> {
    let mut operation: UpgradeOperation = match read_json(&data_dir.join(JOURNAL)) {
        Ok(operation) => operation,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if let UpgradeProgress::Active { stage } = operation.progress
        && !coordinator_active(data_dir)?
    {
        operation.progress = UpgradeProgress::Failed {
            failure: failed(
                stage,
                UpgradeFailureKind::OwnerInterrupted,
                "upgrade coordinator exited before recording readiness; inspect the daemon and retry explicitly",
            ),
        };
    }
    Ok(Some(operation))
}

/// Inspection is read-only with respect to daemon/store lifecycle. The selected
/// executable may itself be missing or incompatible; status still reports it.
pub async fn status(spec: &DaemonSpec) -> io::Result<ManagedDaemonStatus> {
    let installed = match crate::build::inspect(&spec.program).await {
        Ok(candidate) => InstalledCandidate::Available {
            build: candidate.descriptor,
        },
        Err(error) => InstalledCandidate::Unavailable {
            reason: error.to_string(),
        },
    };
    let running = match control::inspect(&spec.socket).await {
        Ok(daemon) => ManagedDaemonState::Running {
            build: daemon.build,
        },
        Err(control_error) => match launch::availability(spec).await? {
            launch::Availability::Stopped => ManagedDaemonState::Stopped {},
            launch::Availability::Listening => ManagedDaemonState::Legacy {
                reason: format!("endpoint has no supported maintenance control: {control_error}"),
            },
            launch::Availability::Transitioning { phase } => ManagedDaemonState::Unavailable {
                reason: format!("daemon still owns its store ({phase:?})"),
            },
        },
    };
    Ok(ManagedDaemonStatus {
        running,
        installed,
        operation: endpoint_operation(&spec.socket)?,
    })
}

/// A journal belongs to the selected endpoint only when its recorded runtime
/// agrees; a configured default store never supplies another context's status.
fn endpoint_operation(socket: &Path) -> io::Result<Option<UpgradeOperation>> {
    let Some(data_dir) = ownership::associated_data_dir(socket)? else {
        return Ok(None);
    };
    let plan: Plan = match read_json(&data_dir.join(ACTIVE_PLAN)) {
        Ok(plan) => plan,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if same_endpoint(socket, &plan.runtime.socket)? {
        operation(&data_dir)
    } else {
        Ok(None)
    }
}

fn same_endpoint(left: &Path, right: &Path) -> io::Result<bool> {
    fn normalize(path: &Path) -> io::Result<PathBuf> {
        let path = ownership::socket_target(path)?;
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let name = path.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "socket needs a filename")
        })?;
        Ok(std::fs::canonicalize(parent)?.join(name))
    }
    Ok(normalize(left)? == normalize(right)?)
}

fn allow(
    candidate: &BuildDescriptor,
    running: &BuildDescriptor,
    intent: UpgradeIntent,
) -> Result<(), UpgradeFailure> {
    let kind = match candidate.release.relative_to(&running.release) {
        ReleaseRelation::Newer => None,
        ReleaseRelation::Equal if intent == UpgradeIntent::Explicit => None,
        ReleaseRelation::Equal => Some(UpgradeFailureKind::UnorderedRelease),
        ReleaseRelation::Older => Some(UpgradeFailureKind::OlderRelease),
        ReleaseRelation::DifferentChannel => Some(UpgradeFailureKind::DifferentChannel),
    };
    if let Some(kind) = kind {
        return Err(failed(
            UpgradeStage::PreparingRestart,
            kind,
            "installed build is not a newer release on the running channel; an old reconnecting client must not replace a newer daemon",
        ));
    }
    if candidate.schema < running.schema {
        return Err(failed(
            UpgradeStage::PreparingRestart,
            UpgradeFailureKind::MigrationFailed,
            "installed build cannot read the running daemon's store schema",
        ));
    }
    Ok(())
}

/// Activate only the installed program selected by this managed context. The
/// actual incumbent supplies its data/socket/listener/idle settings.
pub async fn begin(spec: &DaemonSpec, intent: UpgradeIntent) -> UpgradeResult {
    match begin_inner(spec, intent, None).await {
        Ok(result) => result,
        Err(failure) => UpgradeResult::Failed { failure },
    }
}

/// Activate the exact candidate already inspected by a caller coordinating a
/// compatible worker handoff. Installation changes fail before disruption.
pub async fn begin_verified(
    spec: &DaemonSpec,
    intent: UpgradeIntent,
    expected: nits_protocol::BuildDigest,
) -> UpgradeResult {
    match begin_inner(spec, intent, Some(expected)).await {
        Ok(result) => result,
        Err(failure) => UpgradeResult::Failed { failure },
    }
}

async fn begin_inner(
    spec: &DaemonSpec,
    intent: UpgradeIntent,
    expected: Option<nits_protocol::BuildDigest>,
) -> Result<UpgradeResult, UpgradeFailure> {
    let stage = UpgradeStage::PreparingRestart;
    let candidate = crate::build::inspect(&spec.program)
        .await
        .map_err(|error| {
            failed(
                stage,
                UpgradeFailureKind::CandidateUnavailable,
                error.to_string(),
            )
        })?;
    if expected.is_some_and(|digest| digest != candidate.descriptor.digest) {
        return Err(failed(
            stage,
            UpgradeFailureKind::ContextMismatch,
            "installed candidate changed since preflight; the incumbent was not disrupted. Inspect the new build before retrying",
        ));
    }
    let existing = endpoint_operation(&spec.socket).map_err(|error| io_failure(stage, &error))?;
    if let Some(result) = join_active(existing.as_ref(), &candidate.descriptor)? {
        return Ok(result);
    }
    let running = control::inspect(&spec.socket).await.map_err(|error| {
        failed(
            stage,
            UpgradeFailureKind::IncompatibleControl,
            format!("{error}; legacy daemons require the documented bounded stop/start bootstrap"),
        )
    })?;
    if candidate.descriptor.digest == running.build.digest {
        return Ok(UpgradeResult::AlreadyCurrent {
            build: running.build,
        });
    }
    allow(&candidate.descriptor, &running.build, intent)?;
    let data_dir = std::fs::canonicalize(&running.runtime.data_dir)
        .map_err(|error| io_failure(stage, &error))?;
    let path = candidate.freeze(&data_dir).map_err(|error| {
        failed(
            stage,
            UpgradeFailureKind::CandidateUnavailable,
            error.to_string(),
        )
    })?;
    let (ts, random) = crate::ids::fresh_parts();
    let requested_operation = UpgradeOperation {
        id: UpgradeId::from_parts(ts, random),
        source: running.build,
        target: candidate.descriptor.clone(),
        progress: UpgradeProgress::Active { stage },
    };
    let prior = operation(&data_dir).map_err(|error| io_failure(stage, &error))?;
    // Admission can race the earlier endpoint check while inspecting/freezing
    // the candidate. Join or refuse now; never spawn behind a known active job.
    if let Some(result) = join_active(prior.as_ref(), &candidate.descriptor)? {
        return Ok(result);
    }
    let plan = Plan {
        operation: requested_operation,
        preflight_operation: prior.as_ref().map(|operation| operation.id),
        runtime: running.runtime,
        program: path.clone(),
        installed: candidate.program,
        intent,
    };
    let plan_path = data_dir.join(format!("upgrade-request-{}.json", plan.operation.id));
    write_json(&plan_path, &plan).map_err(|error| io_failure(stage, &error))?;
    let mut command = detached_command(&path);
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(data_dir.join("nitsd.log"))
        .map_err(|error| io_failure(stage, &error))?;
    command
        .args(["daemon", "upgrade-run", "--plan"])
        .arg(&plan_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    if let Err(error) = command.spawn() {
        let _ = std::fs::remove_file(plan_path);
        return Err(failed(
            stage,
            UpgradeFailureKind::StartFailed,
            error.to_string(),
        ));
    }
    observe_operation(&data_dir, &plan).await
}

/// Active operations are joined only by callers selecting the same full build.
/// Check both before inspecting the incumbent and immediately before spawning.
fn join_active(
    operation: Option<&UpgradeOperation>,
    target: &BuildDescriptor,
) -> Result<Option<UpgradeResult>, UpgradeFailure> {
    let Some(operation) = operation else {
        return Ok(None);
    };
    if !matches!(operation.progress, UpgradeProgress::Active { .. }) {
        return Ok(None);
    }
    if operation.target == *target {
        Ok(Some(UpgradeResult::Accepted {
            operation: operation.clone(),
        }))
    } else {
        Err(failed(
            UpgradeStage::PreparingRestart,
            UpgradeFailureKind::ContextMismatch,
            "another installed build is already being activated for this endpoint; inspect its operation before retrying",
        ))
    }
}

async fn observe_operation(data_dir: &Path, plan: &Plan) -> Result<UpgradeResult, UpgradeFailure> {
    let stage = UpgradeStage::PreparingRestart;
    let deadline = tokio::time::Instant::now() + CALL_BUDGET;
    loop {
        match read_json::<RequestOutcome>(&outcome_path(data_dir, plan.operation.id)) {
            Ok(RequestOutcome::Failed { failure }) => return Err(failure),
            Ok(outcome) => return Ok(outcome.into()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_failure(stage, &error)),
        }
        if let Some(operation) = operation(data_dir).map_err(|error| io_failure(stage, &error))?
            && (Some(operation.id) != plan.preflight_operation || operation.id == plan.operation.id)
        {
            if operation.target != plan.operation.target {
                return Err(failed(
                    stage,
                    UpgradeFailureKind::ContextMismatch,
                    "another installed build won activation after preflight; inspect its operation before retrying",
                ));
            }
            match &operation.progress {
                UpgradeProgress::Ready {} => return Ok(UpgradeResult::Restarted { operation }),
                UpgradeProgress::Failed { failure } => return Err(failure.clone()),
                UpgradeProgress::Active { .. } if tokio::time::Instant::now() >= deadline => {
                    return Ok(UpgradeResult::Accepted { operation });
                }
                UpgradeProgress::Active { .. } => {}
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(failed(
                stage,
                UpgradeFailureKind::StartFailed,
                "coordinator readiness was not confirmed within the call budget; inspect upgrade-status before retrying",
            ));
        }
        tokio::time::sleep(POLL).await;
    }
}

fn detached_command(program: &Path) -> Command {
    // nohup execs the selected binary; no shell interpretation is involved.
    if Command::new("nohup")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
    {
        let mut command = Command::new("nohup");
        command.arg(program);
        command
    } else {
        Command::new(program)
    }
}

/// Private process entry point. The stable lock serializes all target requests,
/// including canonical/symlink paths, and is never unlinked or replaced.
pub async fn run(plan_path: &Path) -> io::Result<()> {
    let plan: Plan = read_json(plan_path)?;
    std::fs::remove_file(plan_path)?;
    let data_dir = std::fs::canonicalize(&plan.runtime.data_dir)?;
    let _owner = lock(&data_dir).await?;
    let request = plan.operation.id;
    let outcome = match revalidate(&data_dir, &plan).await {
        Err(failure) => RequestOutcome::Failed { failure },
        Ok(Revalidated::Complete(outcome)) => outcome,
        Ok(Revalidated::Activate) => match ActivePlan::publish(&data_dir, plan) {
            Err(failure) => RequestOutcome::Failed { failure },
            Ok(mut active) => {
                let result = run_owned(&data_dir, &mut active).await;
                active.finish(&data_dir, result)?
            }
        },
    };
    write_json(&outcome_path(&data_dir, request), &outcome)
}

async fn revalidate(data_dir: &Path, plan: &Plan) -> Result<Revalidated, UpgradeFailure> {
    let preparing = UpgradeStage::PreparingRestart;
    // Revalidate after acquiring ownership: another upgrader may have won.
    let running = control::inspect(&plan.runtime.socket)
        .await
        .map_err(|error| io_failure(preparing, &error))?;
    if running.build.digest == plan.operation.target.digest {
        if let Ok(existing) = read_json::<UpgradeOperation>(&data_dir.join(JOURNAL))
            && existing.target.digest == plan.operation.target.digest
            && matches!(existing.progress, UpgradeProgress::Ready {})
        {
            return Ok(Revalidated::Complete(RequestOutcome::Restarted {
                operation: existing,
            }));
        }
        return Ok(Revalidated::Complete(RequestOutcome::AlreadyCurrent {
            build: running.build,
        }));
    }
    let current = operation(data_dir).map_err(|error| io_failure(preparing, &error))?;
    if current.as_ref().map(|operation| operation.id) != plan.preflight_operation {
        if let Some(UpgradeOperation {
            target,
            progress: UpgradeProgress::Failed { failure },
            ..
        }) = current
            && target == plan.operation.target
        {
            return Err(failure);
        }
        return Err(failed(
            preparing,
            UpgradeFailureKind::ContextMismatch,
            "another operation was admitted after preflight; this queued request cannot activate. Inspect current status and retry explicitly",
        ));
    }
    allow(&plan.operation.target, &running.build, plan.intent)?;
    if running.runtime != plan.runtime || running.build != plan.operation.source {
        return Err(failed(
            preparing,
            UpgradeFailureKind::ContextMismatch,
            "incumbent changed during preflight; retry against its current status",
        ));
    }
    let candidate = crate::build::inspect(&plan.program)
        .await
        .map_err(|error| {
            failed(
                preparing,
                UpgradeFailureKind::CandidateUnavailable,
                error.to_string(),
            )
        })?;
    if candidate.descriptor != plan.operation.target {
        return Err(failed(
            preparing,
            UpgradeFailureKind::CandidateUnavailable,
            "frozen replacement does not match the planned build",
        ));
    }
    Ok(Revalidated::Activate)
}

impl ActivePlan {
    fn publish(data_dir: &Path, mut plan: Plan) -> Result<Self, UpgradeFailure> {
        let preparing = UpgradeStage::PreparingRestart;
        write_json(&data_dir.join(ACTIVE_PLAN), &plan)
            .map_err(|error| io_failure(preparing, &error))?;
        publish(data_dir, &mut plan.operation, preparing)?;
        Ok(Self(plan))
    }

    fn finish(
        mut self,
        data_dir: &Path,
        result: Result<(), UpgradeFailure>,
    ) -> io::Result<RequestOutcome> {
        let outcome = match result {
            Ok(()) => {
                self.0.operation.progress = UpgradeProgress::Ready {};
                RequestOutcome::Restarted {
                    operation: self.0.operation.clone(),
                }
            }
            Err(failure) => {
                self.0.operation.progress = UpgradeProgress::Failed {
                    failure: failure.clone(),
                };
                RequestOutcome::Failed { failure }
            }
        };
        write_json(&data_dir.join(JOURNAL), &self.0.operation)?;
        Ok(outcome)
    }
}

async fn run_owned(data_dir: &Path, active: &mut ActivePlan) -> Result<(), UpgradeFailure> {
    let plan = &mut active.0;
    let preparing = UpgradeStage::PreparingRestart;
    match control::request(
        &plan.runtime.socket,
        control::Request::Prepare {
            operation: plan.operation.clone(),
        },
    )
    .await
    .map_err(|error| failed(UpgradeStage::Draining, UpgradeFailureKind::Io,
        format!("restart preparation acknowledgement was lost ({error}); the incumbent may be draining. No replacement was launched. Inspect status and use daemon start only after ownership is released")))?
    {
        control::Response::Prepared { operation } if operation.target == plan.operation.target => {
            plan.operation = operation;
        }
        control::Response::Prepared { .. } | control::Response::Running { .. } => {
            return Err(failed(
                preparing,
                UpgradeFailureKind::ContextMismatch,
                "incumbent is draining for a different replacement",
            ));
        }
        control::Response::Rejected { reason } => {
            return Err(failed(
                preparing,
                UpgradeFailureKind::ContextMismatch,
                reason,
            ));
        }
    }
    wait_for_release(data_dir, plan).await?;
    start_replacement(data_dir, plan).await
}

async fn wait_for_release(data_dir: &Path, plan: &mut Plan) -> Result<(), UpgradeFailure> {
    publish(data_dir, &mut plan.operation, UpgradeStage::Draining)?;
    let deadline = tokio::time::Instant::now() + DRAIN_TIMEOUT;
    loop {
        let free = ownership::probe(data_dir)
            .map_err(|error| io_failure(UpgradeStage::Draining, &error))?
            == Ownership::Free;
        if free && !launch::is_listening(&plan.runtime.socket).await {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(failed(
                UpgradeStage::Draining,
                UpgradeFailureKind::DrainTimeout,
                "accepted work still owns the store; no replacement was started or work killed. Wait for work to finish, inspect status, then use daemon start to recover the stopped endpoint",
            ));
        }
        tokio::time::sleep(POLL).await;
    }
    Ok(())
}

async fn start_replacement(data_dir: &Path, plan: &mut Plan) -> Result<(), UpgradeFailure> {
    publish(
        data_dir,
        &mut plan.operation,
        UpgradeStage::StartingReplacement,
    )?;
    let spec = DaemonSpec {
        program: plan.program.clone(),
        data_dir: plan.runtime.data_dir.clone(),
        socket: plan.runtime.socket.clone(),
        idle_exit: plan.runtime.idle_exit,
        ws: plan.runtime.ws,
        ..DaemonSpec::for_data_dir(data_dir.to_path_buf())
    };
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(data_dir.join("nitsd.log"))
        .map_err(|error| io_failure(UpgradeStage::StartingReplacement, &error))?;
    let mut child = detached_command(&spec.program)
        .args(spec.args())
        .env(START_OPERATION_ENV, plan.operation.id.to_string())
        .env(launch::NITS_BIN_ENV, &plan.installed)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .map_err(|error| {
            failed(
                UpgradeStage::StartingReplacement,
                UpgradeFailureKind::StartFailed,
                error.to_string(),
            )
        })?;
    publish(data_dir, &mut plan.operation, UpgradeStage::ConfirmingReady)?;
    let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
    loop {
        if let Ok(running) = control::inspect(&spec.socket).await
            && running.build == plan.operation.target
        {
            let (ts, random) = crate::ids::fresh_parts();
            let identity = crate::client::Identity {
                client_id: nits_protocol::ClientId::from_parts(ts, random),
                client: nits_protocol::BuildInfo {
                    name: "nits-upgrade".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                },
                author: nits_protocol::Author::Daemon {
                    machine: gethostname::gethostname().to_string_lossy().into_owned(),
                },
            };
            if let Ok(Ok(client)) = tokio::time::timeout(
                Duration::from_secs(2),
                crate::client::Client::connect_unix(&spec.socket, identity),
            )
            .await
                && client.welcome.protocol == plan.operation.target.protocol
            {
                return Ok(());
            }
        }
        if let Some(exit) = child
            .try_wait()
            .map_err(|error| io_failure(UpgradeStage::StartingReplacement, &error))?
        {
            let failure = read_json::<UpgradeFailure>(&data_dir.join(format!("upgrade-start-{}.json",plan.operation.id))).unwrap_or_else(|_|failed(UpgradeStage::StartingReplacement,UpgradeFailureKind::StartFailed,format!("replacement exited {exit}; inspect {}. No automatic rollback was attempted",data_dir.join("nitsd.log").display())));
            return Err(failure);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(failed(
                UpgradeStage::ConfirmingReady,
                UpgradeFailureKind::ReadinessTimeout,
                "replacement did not complete its expected handshake; it was not killed or rolled back. Inspect status and the daemon log",
            ));
        }
        tokio::time::sleep(POLL).await;
    }
}

fn publish(
    data_dir: &Path,
    operation: &mut UpgradeOperation,
    stage: UpgradeStage,
) -> Result<(), UpgradeFailure> {
    operation.progress = UpgradeProgress::Active { stage };
    write_json(&data_dir.join(JOURNAL), operation).map_err(|error| io_failure(stage, &error))
}

/// Typed startup failure from the actual replacement, never guessed from logs.
pub fn record_start_failure(data_dir: &Path, error: &anyhow::Error) -> io::Result<()> {
    let Some(id) = std::env::var_os(START_OPERATION_ENV) else {
        return Ok(());
    };
    let id: UpgradeId = id.to_string_lossy().parse().map_err(io::Error::other)?;
    let migration = error.chain().any(is_migration_error);
    let failure = failed(
        UpgradeStage::StartingReplacement,
        if migration {
            UpgradeFailureKind::MigrationFailed
        } else {
            UpgradeFailureKind::StartFailed
        },
        format!("{error:#}"),
    );
    write_json(&data_dir.join(format!("upgrade-start-{id}.json")), &failure)
}

fn is_migration_error(error: &(dyn std::error::Error + 'static)) -> bool {
    use nits_review_core::{CoreError, store::StoreError};
    // Transparent thiserror wrappers delegate `source()` to the wrapped
    // error's source. Inspect those typed wrappers too: a source-less
    // SchemaTooNew otherwise disappears from anyhow's chain entirely.
    let store = if let Some(crate::daemon::DaemonError::Core(CoreError::Store(store))) =
        error.downcast_ref::<crate::daemon::DaemonError>()
    {
        Some(store)
    } else if let Some(CoreError::Store(store)) = error.downcast_ref::<CoreError>() {
        Some(store)
    } else {
        error.downcast_ref::<StoreError>()
    };
    matches!(
        store,
        Some(StoreError::SchemaTooNew { .. } | StoreError::Migration(_))
    )
}

/// Automatic repair needs both a newer release and a candidate this caller can
/// actually speak. Old reconnecting clients never activate their own older code.
pub async fn repair_incompatible(spec: &DaemonSpec) -> io::Result<()> {
    let Ok(running) = control::inspect(&spec.socket).await else {
        return Ok(());
    };
    if running.build.protocol == nits_protocol::ProtocolVersion::CURRENT {
        return Ok(());
    }
    let candidate = crate::build::inspect(&spec.program).await?;
    if candidate.descriptor.protocol != nits_protocol::ProtocolVersion::CURRENT
        || candidate
            .descriptor
            .release
            .relative_to(&running.build.release)
            != ReleaseRelation::Newer
    {
        return Ok(());
    }
    match begin(spec, UpgradeIntent::Automatic).await {
        UpgradeResult::AlreadyCurrent { .. } | UpgradeResult::Restarted { .. } => Ok(()),
        UpgradeResult::Failed { failure } => Err(io::Error::other(format!(
            "upgrade failed at {:?}: {}",
            failure.stage, failure.message
        ))),
        UpgradeResult::Accepted {
            operation: accepted,
        } => {
            let deadline = tokio::time::Instant::now() + DRAIN_TIMEOUT + READY_TIMEOUT;
            loop {
                if let Some(current) = operation(&running.runtime.data_dir)?
                    && current.id == accepted.id
                {
                    match current.progress {
                        UpgradeProgress::Ready {} => return Ok(()),
                        UpgradeProgress::Failed { failure } => {
                            return Err(io::Error::other(format!(
                                "upgrade failed at {:?}: {}",
                                failure.stage, failure.message
                            )));
                        }
                        UpgradeProgress::Active { .. } => {}
                    }
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "upgrade remains in progress; inspect daemon upgrade-status",
                    ));
                }
                tokio::time::sleep(POLL).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct RunningFixture {
        directory: tempfile::TempDir,
        runtime: crate::serve::ServeOpts,
        daemon: std::sync::Arc<crate::Daemon>,
        task: tokio::task::JoinHandle<()>,
        build: BuildDescriptor,
    }

    impl RunningFixture {
        fn new() -> Self {
            let directory = tempfile::tempdir().unwrap();
            let runtime = crate::serve::ServeOpts::new(directory.path().to_path_buf());
            let daemon = crate::Daemon::open_at_socket(
                &nits_review_core::DataDir::new(directory.path()),
                nits_protocol::BuildInfo {
                    name: "coordinator-test".into(),
                    version: "1".into(),
                },
                &runtime.socket,
            )
            .unwrap();
            let server = control::Server::bind(runtime.clone()).unwrap();
            let task = tokio::spawn(server.run(std::sync::Arc::clone(&daemon)));
            Self {
                directory,
                runtime,
                daemon,
                task,
                build: crate::build::running().unwrap(),
            }
        }

        fn plan(&self, id: u128, source: BuildDescriptor, target: BuildDescriptor) -> Plan {
            Plan {
                operation: UpgradeOperation {
                    id: UpgradeId::from_parts(1, id),
                    source,
                    target,
                    progress: UpgradeProgress::Active {
                        stage: UpgradeStage::PreparingRestart,
                    },
                },
                preflight_operation: operation(self.directory.path())
                    .unwrap()
                    .map(|operation| operation.id),
                runtime: self.runtime.clone(),
                program: self.directory.path().join("missing-frozen-candidate"),
                installed: self.directory.path().join("installation"),
                intent: UpgradeIntent::Explicit,
            }
        }

        fn winner(&self, target: BuildDescriptor) -> Plan {
            let mut plan = self.plan(1, self.changed_build(1), target);
            plan.operation.progress = UpgradeProgress::Ready {};
            write_json(&self.directory.path().join(ACTIVE_PLAN), &plan).unwrap();
            write_json(&self.directory.path().join(JOURNAL), &plan.operation).unwrap();
            plan
        }

        fn changed_build(&self, byte: u8) -> BuildDescriptor {
            let mut build = self.build.clone();
            build.digest = nits_protocol::BuildDigest::from_bytes([byte; 32]);
            build
        }

        async fn complete(&self, plan: &Plan) {
            let path = self
                .directory
                .path()
                .join(format!("request-{}", plan.operation.id));
            write_json(&path, plan).unwrap();
            tokio::time::timeout(Duration::from_secs(2), run(&path))
                .await
                .unwrap()
                .unwrap();
        }
    }

    impl Drop for RunningFixture {
        fn drop(&mut self) {
            self.daemon.shutdown().cancel();
            self.task.abort();
        }
    }

    #[tokio::test]
    async fn refused_contenders_keep_the_winners_journal_and_their_own_precise_failure() {
        let fixture = RunningFixture::new();
        let winner = fixture.winner(fixture.build.clone());
        let files = [ACTIVE_PLAN, JOURNAL].map(|name| {
            let path = fixture.directory.path().join(name);
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes)
        });
        for (id, source, expected) in [
            (
                2,
                fixture.changed_build(1),
                UpgradeFailureKind::ContextMismatch,
            ),
            (
                3,
                fixture.build.clone(),
                UpgradeFailureKind::CandidateUnavailable,
            ),
        ] {
            let plan = fixture.plan(id, source, fixture.changed_build(3));
            fixture.complete(&plan).await;
            for (path, bytes) in &files {
                assert_eq!(&std::fs::read(path).unwrap(), bytes);
            }
            let error = tokio::time::timeout(
                Duration::from_secs(1),
                observe_operation(fixture.directory.path(), &plan),
            )
            .await
            .unwrap()
            .unwrap_err();
            assert_eq!(error.kind, expected);
            assert_eq!(
                operation(fixture.directory.path()).unwrap(),
                Some(winner.operation.clone())
            );
            assert_eq!(
                control::inspect(&fixture.runtime.socket)
                    .await
                    .unwrap()
                    .build,
                fixture.build
            );
            assert!(fixture.daemon.lifecycle().borrow().is_none());
            assert!(!fixture.daemon.shutdown().is_cancelled());
        }
    }

    #[tokio::test]
    async fn queued_same_target_joins_readiness_or_reports_current_without_republishing() {
        let fixture = RunningFixture::new();
        for (id, previous_target) in [(2, fixture.build.clone()), (3, fixture.changed_build(4))] {
            let plan = fixture.plan(id, fixture.changed_build(1), fixture.build.clone());
            let winner = fixture.winner(previous_target);
            let files = [ACTIVE_PLAN, JOURNAL].map(|name| {
                let path = fixture.directory.path().join(name);
                let bytes = std::fs::read(&path).unwrap();
                (path, bytes)
            });
            fixture.complete(&plan).await;
            let result = observe_operation(fixture.directory.path(), &plan)
                .await
                .unwrap();
            if winner.operation.target == fixture.build {
                assert_eq!(
                    result,
                    UpgradeResult::Restarted {
                        operation: winner.operation
                    }
                );
            } else {
                assert_eq!(
                    result,
                    UpgradeResult::AlreadyCurrent {
                        build: fixture.build.clone()
                    }
                );
            }
            for (path, bytes) in &files {
                assert_eq!(&std::fs::read(path).unwrap(), bytes);
            }
        }
    }

    #[tokio::test]
    async fn a_distinct_new_winner_rejects_the_observing_contender_without_waiting_for_its_child() {
        let fixture = RunningFixture::new();
        let contender = fixture.plan(2, fixture.changed_build(1), fixture.changed_build(3));
        let winner = fixture.winner(fixture.build.clone());
        let error = tokio::time::timeout(
            Duration::from_secs(1),
            observe_operation(fixture.directory.path(), &contender),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(error.kind, UpgradeFailureKind::ContextMismatch);
        assert_eq!(
            operation(fixture.directory.path()).unwrap(),
            Some(winner.operation)
        );
        assert!(!outcome_path(fixture.directory.path(), contender.operation.id).exists());
    }

    #[tokio::test]
    async fn failed_admission_cannot_activate_a_refused_or_coalesced_child_but_allows_a_new_retry()
    {
        for same_target in [false, true] {
            let fixture = RunningFixture::new();
            let contender = fixture.plan(2, fixture.build.clone(), fixture.changed_build(2));
            let target = if same_target {
                contender.operation.target.clone()
            } else {
                fixture.changed_build(3)
            };
            let mut winner = fixture.winner(target);
            let rejection = failed(
                UpgradeStage::PreparingRestart,
                UpgradeFailureKind::Io,
                "preparation failed while the original daemon remained live",
            );
            winner.operation.progress = UpgradeProgress::Failed {
                failure: rejection.clone(),
            };
            write_json(&fixture.directory.path().join(JOURNAL), &winner.operation).unwrap();
            let files = [ACTIVE_PLAN, JOURNAL].map(|name| {
                let path = fixture.directory.path().join(name);
                let bytes = std::fs::read(&path).unwrap();
                (path, bytes)
            });
            let expected = if same_target {
                UpgradeFailureKind::Io
            } else {
                UpgradeFailureKind::ContextMismatch
            };
            // The caller has already received a definitive refusal/failure.
            let observed = observe_operation(fixture.directory.path(), &contender)
                .await
                .unwrap_err();
            assert_eq!(observed.kind, expected);
            if same_target {
                assert_eq!(observed, rejection);
            }
            // Its detached child obtains the lock afterward. The unchanged
            // incumbent is insufficient permission to activate or even inspect
            // the missing candidate; the intervening admission is terminal.
            fixture.complete(&contender).await;
            let completed = observe_operation(fixture.directory.path(), &contender)
                .await
                .unwrap_err();
            assert_eq!(completed.kind, expected);
            if same_target {
                assert_eq!(completed, rejection);
            }
            // A new explicit call made after the failure is allowed to perform
            // preflight again. It reaches candidate inspection, which precisely
            // reports this fixture's intentionally missing executable.
            let retry = fixture.plan(3, fixture.build.clone(), contender.operation.target);
            fixture.complete(&retry).await;
            assert_eq!(
                observe_operation(fixture.directory.path(), &retry)
                    .await
                    .unwrap_err()
                    .kind,
                UpgradeFailureKind::CandidateUnavailable
            );
            for (path, bytes) in files {
                assert_eq!(std::fs::read(path).unwrap(), bytes);
            }
            assert_eq!(
                control::inspect(&fixture.runtime.socket)
                    .await
                    .unwrap()
                    .build,
                fixture.build
            );
            assert!(fixture.daemon.lifecycle().borrow().is_none());
            assert!(!fixture.daemon.shutdown().is_cancelled());
        }
    }

    fn descriptor(version: &str) -> BuildDescriptor {
        let mut build = crate::build::running().unwrap();
        build.release.version = version.parse().unwrap();
        build
    }

    #[test]
    fn automatic_ordering_and_explicit_activation_cannot_downgrade_or_cross_channels() {
        let running = descriptor("2.0.0");
        for intent in [UpgradeIntent::Automatic, UpgradeIntent::Explicit] {
            assert_eq!(
                allow(&descriptor("1.0.0"), &running, intent)
                    .unwrap_err()
                    .kind,
                UpgradeFailureKind::OlderRelease
            );
            let mut other = descriptor("3.0.0");
            other.release.channel = "preview".parse().unwrap();
            assert_eq!(
                allow(&other, &running, intent).unwrap_err().kind,
                UpgradeFailureKind::DifferentChannel
            );
        }
        assert!(allow(&descriptor("2.0.0"), &running, UpgradeIntent::Automatic).is_err());
        assert!(allow(&descriptor("2.0.0"), &running, UpgradeIntent::Explicit).is_ok());
        assert!(allow(&descriptor("2.0.1"), &running, UpgradeIntent::Automatic).is_ok());
        let mut candidate = descriptor("3.0.0");
        candidate.schema = nits_protocol::SchemaVersion::new(0);
        assert_eq!(
            allow(&candidate, &running, UpgradeIntent::Automatic)
                .unwrap_err()
                .kind,
            UpgradeFailureKind::MigrationFailed
        );
    }

    #[tokio::test]
    async fn stable_lock_aliases_and_journal_report_interrupted_ownership_without_mutating_it() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let alias = dir
            .path()
            .with_extension(format!("alias-{}", fastrand::u64(..)));
        std::os::unix::fs::symlink(dir.path(), &alias).unwrap();
        let _cleanup = AliasCleanup(alias.clone());
        assert!(!coordinator_active(dir.path()).unwrap());
        assert!(!dir.path().join(LOCK).exists());
        let owner = lock(dir.path()).await.unwrap();
        let inode = std::fs::metadata(dir.path().join(LOCK)).unwrap().ino();
        assert!(coordinator_active(&alias).unwrap());
        let operation = UpgradeOperation {
            id: UpgradeId::from_parts(1, 1),
            source: descriptor("1.0.0"),
            target: descriptor("2.0.0"),
            progress: UpgradeProgress::Active {
                stage: UpgradeStage::Draining,
            },
        };
        write_json(&dir.path().join(JOURNAL), &operation).unwrap();
        assert_eq!(super::operation(&alias).unwrap(), Some(operation.clone()));
        drop(owner);
        let stopped = super::operation(dir.path()).unwrap().unwrap();
        assert!(matches!(
            stopped.progress,
            UpgradeProgress::Failed {
                failure: UpgradeFailure {
                    kind: UpgradeFailureKind::OwnerInterrupted,
                    ..
                }
            }
        ));
        assert_eq!(
            read_json::<UpgradeOperation>(&dir.path().join(JOURNAL)).unwrap(),
            operation
        );
        let _next = lock(&alias).await.unwrap();
        assert_eq!(
            std::fs::metadata(dir.path().join(LOCK)).unwrap().ino(),
            inode
        );
    }
    #[tokio::test]
    async fn cancelling_an_upgrade_lock_waiter_releases_its_handle_without_replacing_inode() {
        use std::os::unix::fs::MetadataExt;
        let dir = tempfile::tempdir().unwrap();
        let owner = lock(dir.path()).await.unwrap();
        let inode = std::fs::metadata(dir.path().join(LOCK)).unwrap().ino();
        let mut wait = Box::pin(lock(dir.path()));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut wait)
                .await
                .is_err()
        );
        drop(wait);
        drop(owner);
        let owner = tokio::time::timeout(Duration::from_secs(1), lock(dir.path()))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            std::fs::metadata(dir.path().join(LOCK)).unwrap().ino(),
            inode
        );
        drop(owner);
        assert!(!coordinator_active(dir.path()).unwrap());
    }

    struct AliasCleanup(PathBuf);
    impl Drop for AliasCleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}
