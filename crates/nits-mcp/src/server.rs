//! The MCP server: `initialize`, `tools/list`, `tools/call`, proxied to the
//! daemon through [`nitsd::ops::Ops`]. Calls establish their context and author
//! in input order. Only acknowledged event waits overlap subsequent calls.

use std::path::{Path, PathBuf};
use std::time::Duration;

use nits_protocol::{
    AgentVia, Anchor, Author, BuildInfo, ClientId, CommentKind, Human, LineNo, LineRange, Mutation,
    NonEmpty, RenderContent, RenderOpts, RepoPath, ReviewTarget, Since,
};
use nitsd::client::{Client, ClientError, Identity};
use nitsd::ops::{EventPoll, Ops, OpsError};
use nitsd::render_text as text;
use serde::Deserialize;
use serde_json::{Value, json};
use strum::EnumString;
use tokio_util::sync::CancellationToken;

use crate::jsonrpc::{self, Incoming, Outgoing};
use crate::tools::{
    self, Call, ContextCall, MutatingCall, QueryCall, SessionCall, ToolCall, ToolName,
};

/// JSON-RPC methods this server answers. Anything else is
/// `METHOD_NOT_FOUND`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString)]
enum Method {
    #[strum(serialize = "initialize")]
    Initialize,
    #[strum(serialize = "ping")]
    Ping,
    #[strum(serialize = "tools/list")]
    ToolsList,
    #[strum(serialize = "tools/call")]
    ToolsCall,
    #[strum(serialize = "notifications/cancelled")]
    Cancelled,
}

#[derive(Debug, Deserialize)]
struct Cancellation {
    #[serde(rename = "requestId")]
    request_id: jsonrpc::RequestId,
}

/// The transport schedules only event waits. Every other call has completed
/// before this value is returned, preserving mutation and session ordering.
#[derive(Debug)]
pub(crate) enum Dispatch {
    Reply(Outgoing),
    Notification,
    Cancel(jsonrpc::RequestId),
    Wait {
        id: jsonrpc::RequestId,
        wait: EventWait,
    },
}

#[derive(Debug)]
enum PreparedCall {
    Ready(Result<Value, ToolError>),
    Wait(EventWait),
}

impl PreparedCall {
    async fn finish(self) -> Result<Value, ToolError> {
        match self {
            Self::Ready(result) => result,
            Self::Wait(wait) => wait.finish().await,
        }
    }
}

#[derive(Debug)]
pub(crate) struct EventWait {
    poll: EventPoll,
    context: tools::ContextIdentity,
    session_end: CancellationToken,
}

impl EventWait {
    async fn finish(self) -> Result<Value, ToolError> {
        tokio::select! {
            biased;
            () = self.session_end.cancelled() => Err(ToolError::Invalid(format!(
                "event wait in context {} cancelled because the MCP session connection, context or identity changed",
                self.context.name
            ))),
            polled = self.poll.wait() => {
                let polled = polled?;
                ok(tools::Events { context: self.context, events: polled.events, last_seq: polled.last_seq })
            }
        }
    }

    pub(crate) async fn reply(self, id: &jsonrpc::RequestId) -> Outgoing {
        tool_reply(id.value(), self.finish().await)
    }
}

/// `params` of `tools/call`. `name` is parsed to a `ToolName` here so an
/// unknown tool is a JSON-RPC error, while bad arguments are a tool error.
#[derive(Debug, Deserialize)]
struct CallParams {
    name: ToolName,
    #[serde(default = "empty_object")]
    arguments: Value,
}

fn empty_object() -> Value {
    json!({})
}

/// MCP protocol revision this server implements.
pub const MCP_VERSION: &str = "2025-06-18";

// Include the managed daemon's startup budget, then bound negotiation too.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// How to reach the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub selection: nits_config::Selection,
    /// Context definitions are reloaded for `list_contexts` and `use_context`.
    pub config_path: PathBuf,
    /// Start the daemon when initializing or reconnecting if it is not
    /// running (local and ssh contexts), so an agent can always get going.
    pub start: nitsd::contexts::StartPolicy,
}

/// Identity of the agent on the other end of stdio, from the environment
/// and the MCP `initialize` call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentIdentity {
    /// Model name, e.g. from `NITS_AGENT_MODEL`.
    pub model: String,
    /// Stable id for this agent session, e.g. from `NITS_SESSION_ID`.
    pub session_id: String,
    /// The human whose shell/editor launched the agent, if known.
    pub invoked_by: Option<Human>,
}

impl AgentIdentity {
    /// From `NITS_AGENT_MODEL`, `NITS_SESSION_ID` and `USER`.
    #[must_use]
    pub fn from_env() -> Self {
        let machine = gethostname::gethostname().to_string_lossy().into_owned();
        let (ts, r) = nitsd::ids::fresh_parts();
        Self {
            model: std::env::var("NITS_AGENT_MODEL").unwrap_or_else(|_| "unknown".into()),
            session_id: std::env::var("NITS_SESSION_ID")
                .unwrap_or_else(|_| ClientId::from_parts(ts, r).to_string()),
            invoked_by: std::env::var("USER")
                .ok()
                .map(|name| Human { name, machine }),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("{0}")]
    Invalid(String),
    #[error("not connected: call initialize first")]
    NotInitialized,
    #[error(
        "cannot connect to daemon: {0}; start the daemon or check its context, then call the tool again"
    )]
    Connecting(String),
    #[error(
        "daemon connection closed; this call was not retried and an in-flight mutation may have committed. The next tool call will reconnect; inspect daemon state before repeating a mutation"
    )]
    Disconnected,
    #[error(transparent)]
    Config(#[from] nits_config::ConfigError),
    #[error(transparent)]
    Ops(OpsError),
}

impl From<OpsError> for ToolError {
    fn from(e: OpsError) -> Self {
        match e {
            OpsError::Client(ref source) if matches!(**source, ClientError::Closed) => {
                Self::Disconnected
            }
            other => Self::Ops(other),
        }
    }
}

impl From<ClientError> for ToolError {
    fn from(e: ClientError) -> Self {
        Self::from(OpsError::from(e))
    }
}

impl From<serde_json::Error> for ToolError {
    fn from(e: serde_json::Error) -> Self {
        ToolError::Invalid(format!("invalid params: {e}"))
    }
}

impl From<nits_protocol::InvariantError> for ToolError {
    fn from(e: nits_protocol::InvariantError) -> Self {
        ToolError::Invalid(e.to_string())
    }
}

/// Keep MCP provenance across reconnects, but use a fresh daemon client ID
/// because each new Ops starts its mutation sequence at zero.
#[derive(Debug)]
struct Session {
    author: Author,
    ops: Ops,
    end: CancellationToken,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.end.cancel();
    }
}

/// Legacy bare cursors are unambiguous only before a context switch.
#[derive(Debug, Clone, Copy)]
enum CursorPolicy {
    InitialContext,
    RequireContext,
}

#[derive(Debug)]
pub struct Server {
    endpoint: Endpoint,
    agent: AgentIdentity,
    build: BuildInfo,
    session: Option<Session>,
    cursor_policy: CursorPolicy,
}

impl Server {
    #[must_use]
    pub fn new(endpoint: Endpoint, agent: AgentIdentity, build: BuildInfo) -> Self {
        Self {
            endpoint,
            agent,
            build,
            session: None,
            cursor_policy: CursorPolicy::InitialContext,
        }
    }

    /// The daemon connection, once `initialize` has run.
    #[must_use]
    pub fn client(&self) -> Option<&Client> {
        self.session.as_ref().map(|s| s.ops.client())
    }

    /// Handle one line, awaiting any event wait. The stdio transport uses
    /// `dispatch_line` to keep reading while that wait is pending.
    pub async fn handle_line(&mut self, line: &str) -> Option<Outgoing> {
        finish_dispatch(self.dispatch_line(line).await).await
    }

    /// Handle one decoded message, awaiting any event wait.
    pub async fn handle(&mut self, msg: Incoming) -> Option<Outgoing> {
        finish_dispatch(self.dispatch(msg).await).await
    }

    pub(crate) async fn dispatch_line(&mut self, line: &str) -> Dispatch {
        let msg: Incoming = match serde_json::from_str(line) {
            Ok(m) => m,
            Err(e) => {
                return Dispatch::Reply(Outgoing::error(
                    Value::Null,
                    jsonrpc::PARSE_ERROR,
                    format!("parse error: {e}"),
                ));
            }
        };
        self.dispatch(msg).await
    }

    async fn dispatch(&mut self, msg: Incoming) -> Dispatch {
        if msg.jsonrpc != "2.0" {
            return Dispatch::Reply(Outgoing::error(
                msg.id.unwrap_or(Value::Null),
                jsonrpc::INVALID_REQUEST,
                "jsonrpc must be \"2.0\"",
            ));
        }
        let method = msg.method.parse::<Method>();
        let Some(id) = msg.id else {
            return match method {
                Ok(Method::Cancelled) => serde_json::from_value::<Cancellation>(msg.params)
                    .map_or(Dispatch::Notification, |p| Dispatch::Cancel(p.request_id)),
                _ => Dispatch::Notification,
            };
        };
        let Ok(request_id) = serde_json::from_value::<jsonrpc::RequestId>(id.clone()) else {
            return Dispatch::Reply(Outgoing::error(
                Value::Null,
                jsonrpc::INVALID_REQUEST,
                "request id must be a string or number",
            ));
        };
        let Ok(method) = method else {
            return Dispatch::Reply(Outgoing::error(
                id,
                jsonrpc::METHOD_NOT_FOUND,
                format!("method not found: {}", msg.method),
            ));
        };
        Dispatch::Reply(match method {
            Method::Initialize => match self.initialize(&msg.params).await {
                Ok(v) => Outgoing::result(id, v),
                Err(e) => Outgoing::error(id, jsonrpc::INTERNAL_ERROR, e.to_string()),
            },
            Method::Ping => Outgoing::result(id, json!({})),
            Method::Cancelled => Outgoing::error(
                id,
                jsonrpc::INVALID_REQUEST,
                "cancellation must be a notification",
            ),
            Method::ToolsList => Outgoing::result(
                id,
                json!({
                    "tools": tools::all().into_iter().map(|t| json!({
                        "name": <&'static str>::from(t.name),
                        "description": t.description,
                        "inputSchema": t.input_schema,
                        "outputSchema": t.output_schema,
                    })).collect::<Vec<_>>()
                }),
            ),
            Method::ToolsCall => {
                let params: CallParams = match serde_json::from_value(msg.params) {
                    Ok(p) => p,
                    Err(e) => {
                        return Dispatch::Reply(Outgoing::error(
                            id,
                            jsonrpc::INVALID_PARAMS,
                            format!("invalid tools/call params: {e}"),
                        ));
                    }
                };
                let call = match ToolCall::parse(params.name, params.arguments) {
                    Ok(c) => c,
                    Err(e) => return Dispatch::Reply(tool_reply(id, Err(ToolError::from(e)))),
                };
                match self.prepare_call(call.classify()).await {
                    PreparedCall::Ready(result) => tool_reply(id, result),
                    PreparedCall::Wait(wait) => {
                        return Dispatch::Wait {
                            id: request_id,
                            wait,
                        };
                    }
                }
            }
        })
    }

    async fn initialize(&mut self, params: &Value) -> Result<Value, ToolError> {
        let info = params.get("clientInfo");
        let name = info
            .and_then(|i| i.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("mcp-client")
            .to_string();
        let author = Author::Agent {
            name,
            model: self.agent.model.clone(),
            session_id: self.agent.session_id.clone(),
            invoked_by: self.agent.invoked_by.clone(),
            via: AgentVia::Mcp,
        };
        let client = self.connect(author.clone()).await?;
        let daemon = client.welcome.daemon.clone();
        self.session = Some(Session {
            author,
            ops: Ops::new(client),
            end: CancellationToken::new(),
        });
        Ok(json!({
            "protocolVersion": MCP_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": self.build.name, "version": self.build.version },
            "instructions": format!(
                "Nits code review. Connected to {} {} in context {}. \
                 Use list_contexts and use_context to select a daemon for this session; ordinary calls run in order. Acknowledged event waits may overlap later calls; context or identity changes cancel them. \
                 New sessions follow the CLI persisted default unless launch flags override it. \
                 Reads report their source context. IDs and cursors belong to that context; after switching, \
                 pass since_context with since_seq. Start with list_workspaces, then get_review; \
                 anchor comments with add_comment/suggest; wait for work with subscribe_events. \
                 Use get_session_identity to see your author; set_session_identity with name and \
                 model before posting to identify this agent. Share the returned author.name as \
                 the exact request_review.agent / subscribe_events.awaiting_agent routing key \
                 and keep it stable while collaborating. Identity changes affect only future events.",
                daemon.name, daemon.version, self.endpoint.selection.name
            ),
        }))
    }

    async fn connect(&self, author: Author) -> Result<Client, ToolError> {
        self.connect_context(&self.endpoint.selection.context, author)
            .await
    }

    async fn connect_context(
        &self,
        context: &nits_config::Context,
        author: Author,
    ) -> Result<Client, ToolError> {
        let (ts, r) = nitsd::ids::fresh_parts();
        let identity = Identity {
            client_id: ClientId::from_parts(ts, r),
            client: self.build.clone(),
            author,
        };
        tokio::time::timeout(
            CONNECT_TIMEOUT,
            nitsd::contexts::connect(context, identity, self.endpoint.start),
        )
        .await
        .map_err(|_| ToolError::Connecting(format!("timed out after {CONNECT_TIMEOUT:?}")))?
        .map_err(|e| ToolError::Connecting(e.to_string()))
    }

    fn ops(&self) -> Result<&Ops, ToolError> {
        self.session
            .as_ref()
            .map(|s| &s.ops)
            .ok_or(ToolError::NotInitialized)
    }

    fn ops_mut(&mut self) -> Result<&mut Ops, ToolError> {
        self.session
            .as_mut()
            .map(|s| &mut s.ops)
            .ok_or(ToolError::NotInitialized)
    }

    /// Reconnect before a new tool call, preserving the initialized agent's
    /// provenance. Never replay a call that encountered transport loss: its
    /// mutations may already have committed, even if no response arrived.
    async fn ensure_connected(&mut self) -> Result<(), ToolError> {
        let session = self.session.as_ref().ok_or(ToolError::NotInitialized)?;
        if session.ops.client().is_closed() {
            let author = session.author.clone();
            let client = self.connect(author.clone()).await?;
            self.session = Some(Session {
                author,
                ops: Ops::new(client),
                end: CancellationToken::new(),
            });
        }
        Ok(())
    }

    /// Run a decoded tool call. Public so tests can bypass JSON-RPC.
    pub async fn call(&mut self, call: ToolCall) -> Result<Value, ToolError> {
        self.prepare_call(call.classify()).await.finish().await
    }

    async fn prepare_call(&mut self, call: Call) -> PreparedCall {
        PreparedCall::Ready(match call {
            Call::Query(q) => match self.ensure_connected().await {
                Ok(()) => self.call_query(q).await,
                Err(e) => Err(e),
            },
            Call::EventWait(p) => match self.prepare_wait(p).await {
                Ok(wait) => return PreparedCall::Wait(wait),
                Err(e) => Err(e),
            },
            Call::Mutating(m) => match self.ensure_connected().await {
                Ok(()) => self.call_mutating(m).await,
                Err(e) => Err(e),
            },
            Call::Session(s) => self.call_session(s).await,
            Call::Context(c) => self.call_context(c).await,
        })
    }

    async fn prepare_wait(&mut self, p: tools::SubscribeEvents) -> Result<EventWait, ToolError> {
        // One budget covers reconnecting the session, opening the poll's own
        // connection, and receiving the subscription acknowledgement.
        tokio::time::timeout(CONNECT_TIMEOUT, async {
            self.ensure_connected().await?;
            let since = self.subscription_since(p.start)?;
            let session = self.session.as_ref().ok_or(ToolError::NotInitialized)?;
            // An owned event queue prevents cancellation or one poll's drain
            // from affecting another poll or leaving a shared subscription.
            let client = self.connect(session.author.clone()).await?;
            let poll =
                EventPoll::subscribe(client, p.scope, since, p.timeout.duration(), p.max).await?;
            Ok(EventWait {
                poll,
                context: self.context_identity(),
                session_end: session.end.clone(),
            })
        })
        .await
        .map_err(|_| ToolError::Connecting("event subscription setup timed out".into()))?
    }

    fn context_identity(&self) -> tools::ContextIdentity {
        tools::ContextIdentity {
            name: self.endpoint.selection.name.clone(),
            kind: self.endpoint.selection.context.kind(),
        }
    }

    async fn call_context(&mut self, call: ContextCall) -> Result<Value, ToolError> {
        let session = self.session.as_ref().ok_or(ToolError::NotInitialized)?;
        let config = nits_config::Config::load(&self.endpoint.config_path)?;
        match call {
            ContextCall::List => {
                let mut contexts: Vec<_> = config
                    .contexts
                    .iter()
                    .map(|(name, context)| tools::ContextIdentity {
                        name: name.clone(),
                        kind: context.kind(),
                    })
                    .collect();
                if !config.contexts.contains_key(nits_config::DEFAULT_CONTEXT) {
                    contexts.insert(
                        0,
                        tools::ContextIdentity {
                            name: nits_config::DEFAULT_CONTEXT.parse()?,
                            kind: nits_config::ContextKind::Local,
                        },
                    );
                }
                ok(tools::Contexts {
                    contexts,
                    active: self.context_identity(),
                    persisted: config.current_context,
                })
            }
            ContextCall::Use(p) => {
                let selection =
                    config.selection(Some((&p.name, nits_config::SelectionOrigin::Mcp)))?;
                let author = session.author.clone();
                // Do not touch active state until the new daemon has welcomed us.
                // Dropping the entire old Ops closes subscriptions and queues;
                // a fresh client ID accompanies the new mutation sequence.
                let client = self
                    .connect_context(&selection.context, author.clone())
                    .await?;
                let cursor_policy = if selection.name == self.endpoint.selection.name
                    && selection.context == self.endpoint.selection.context
                {
                    self.cursor_policy
                } else {
                    CursorPolicy::RequireContext
                };
                self.session = Some(Session {
                    author,
                    ops: Ops::new(client),
                    end: CancellationToken::new(),
                });
                self.endpoint.selection = selection;
                self.cursor_policy = cursor_policy;
                ok(tools::ContextSelected {
                    context: self.context_identity(),
                })
            }
        }
    }

    async fn call_session(&mut self, call: SessionCall) -> Result<Value, ToolError> {
        let session = self.session.as_ref().ok_or(ToolError::NotInitialized)?;
        let author = match call {
            SessionCall::GetIdentity => session.author.clone(),
            SessionCall::SetIdentity(p) => {
                let author = Author::Agent {
                    name: p.name,
                    model: p.model,
                    session_id: self.agent.session_id.clone(),
                    invoked_by: self.agent.invoked_by.clone(),
                    via: AgentVia::Mcp,
                };
                // The daemon binds authorship at Hello. Complete negotiation
                // before replacing either author or Ops, so failure is atomic.
                // A fresh client ID also keeps reset client_seq values distinct.
                let client = self.connect(author.clone()).await?;
                self.session = Some(Session {
                    author: author.clone(),
                    ops: Ops::new(client),
                    end: CancellationToken::new(),
                });
                author
            }
        };
        ok(tools::SessionIdentity { author })
    }

    fn subscription_since(&self, start: tools::EventStart) -> Result<Since, ToolError> {
        Ok(match start {
            tools::EventStart::Live => Since::Now,
            tools::EventStart::Replay { seq, source } => {
                match source {
                    tools::CursorSource::Named(name) => {
                        if name != self.endpoint.selection.name {
                            return Err(ToolError::Invalid(format!(
                                "cursor context {name} does not match active context {}; use_context first",
                                self.endpoint.selection.name
                            )));
                        }
                    }
                    tools::CursorSource::InitialContext => {
                        if matches!(self.cursor_policy, CursorPolicy::RequireContext) {
                            return Err(ToolError::Invalid("since_context is required with since_seq after switching contexts; use the context.name that issued the cursor".into()));
                        }
                    }
                }
                Since::After { seq }
            }
        })
    }

    #[allow(clippy::too_many_lines)] // Keep the exhaustive query-to-Core mapping together.
    async fn call_query(&self, call: QueryCall) -> Result<Value, ToolError> {
        let ops = self.ops()?;
        match call {
            QueryCall::ListWorkspaces => ok(tools::Workspaces {
                context: self.context_identity(),
                workspaces: ops.workspaces().await?,
            }),
            QueryCall::ListReviews(p) => {
                let workspace_id = match p.workspace_id {
                    Some(w) => w,
                    None => ops.locate(Path::new(".")).await?.workspace.id,
                };
                ok(tools::Reviews {
                    context: self.context_identity(),
                    reviews: ops.reviews(workspace_id).await?,
                })
            }
            QueryCall::GetReview(p) => {
                let snap = ops.snapshot(p.review_id).await?;
                let files = ops.files(p.review_id).await?;
                ok(tools::ReviewDetail {
                    context: self.context_identity(),
                    review: snap.review,
                    resolved: snap.resolved.clone(),
                    files,
                    threads: snap.threads,
                    comments: snap.comments,
                    latest_checkpoints: nits_protocol::latest_checkpoints(
                        &snap.checkpoints,
                        snap.resolved.as_ref(),
                    ),
                    checkpoints: snap.checkpoints,
                    requests: snap.requests,
                    seq: snap.seq,
                })
            }
            QueryCall::GetCheckpointDelta(p) => {
                let (files, targets) = ops
                    .files_scoped(
                        p.review_id,
                        nits_protocol::DiffScope::SinceCheckpoint {
                            checkpoint_id: p.checkpoint_id,
                        },
                    )
                    .await?;
                ok(tools::CheckpointDelta {
                    context: self.context_identity(),
                    targets,
                    files,
                })
            }
            QueryCall::GetDiff(p) => {
                let render_opts = RenderOpts {
                    ignore_whitespace: p.ignore_whitespace,
                    context_lines: p
                        .context_lines
                        .unwrap_or(RenderOpts::default().context_lines),
                    ..RenderOpts::default()
                };
                let (file, header, chunks) = ops
                    .diff_scoped(p.review_id, p.repo_id, &p.path, render_opts, p.scope)
                    .await?;
                let text = text::render(&header, &chunks);
                ok(tools::DiffText {
                    context: self.context_identity(),
                    repo_id: file.repo_id,
                    path: file.path,
                    change: file.kind,
                    lang: header.lang,
                    content: header.content,
                    text,
                })
            }
            QueryCall::GetFile(p) => self.get_file(p).await,
            QueryCall::ListComments(p) => {
                let snap = ops.snapshot(p.review_id).await?;
                ok(tools::Comments {
                    context: self.context_identity(),
                    threads: snap.threads,
                    comments: snap.comments,
                    latest_checkpoints: nits_protocol::latest_checkpoints(
                        &snap.checkpoints,
                        snap.resolved.as_ref(),
                    ),
                    checkpoints: snap.checkpoints,
                    requests: snap.requests,
                    seq: snap.seq,
                })
            }
        }
    }

    async fn get_file(&self, p: tools::GetFile) -> Result<Value, ToolError> {
        let ops = self.ops()?;
        let path = p.path;
        let (repo_id, blob_oid, header, chunks) =
            ops.file_at(p.review_id, p.repo_id, &path, p.side).await?;
        let lines = match header.content {
            RenderContent::Binary => {
                if p.lines.is_some() {
                    return Err(ToolError::Invalid(
                        "binary files have no source lines; omit start_line and end_line".into(),
                    ));
                }
                None
            }
            RenderContent::Text { total_rows, .. } => {
                let returned_range = match LineNo::new(total_rows) {
                    Some(last) => {
                        let start = p.lines.map_or(LineNo::FIRST, LineRange::start);
                        let end = p.lines.map_or(last, |range| range.end().min(last));
                        if start <= end {
                            Some(LineRange::new(start, end)?)
                        } else {
                            None
                        }
                    }
                    None => None,
                };
                Some(tools::FileLines {
                    total_lines: total_rows,
                    returned_range,
                })
            }
        };
        let text = match p.lines {
            Some(range) => text::render_blob_range(&header, &chunks, range),
            None => text::render_blob(&header, &chunks),
        };
        ok(tools::FileText {
            context: self.context_identity(),
            repo_id,
            path,
            side: p.side,
            blob_oid,
            lang: header.lang,
            content: header.content,
            lines,
            text,
        })
    }

    #[allow(clippy::too_many_lines)] // one exhaustive dispatch over typed mutating calls
    async fn call_mutating(&mut self, call: MutatingCall) -> Result<Value, ToolError> {
        match call {
            MutatingCall::EnsureDirectoryReview(p) => ok(self
                .ops_mut()?
                .ensure_directory_review(p.path, p.base, p.head)
                .await?),
            MutatingCall::UpdateReviewTarget(p) => {
                let event = self
                    .ops_mut()?
                    .mutate(Mutation::UpdateReviewTarget {
                        review_id: p.review_id,
                        update: nits_protocol::ReviewTargetUpdate {
                            repo_id: p.repo_id,
                            revision: p.revision,
                        },
                    })
                    .await?;
                ok(tools::TargetUpdated {
                    review_id: p.review_id,
                    repo_id: p.repo_id,
                    seq: event.seq,
                })
            }
            MutatingCall::CreateReview(p) => self.create_review(p).await,
            MutatingCall::UpdateReview(p) => {
                let event = self
                    .ops_mut()?
                    .mutate(Mutation::UpdateReview {
                        review_id: p.review_id,
                        title: p.title,
                        status: p.status,
                    })
                    .await?;
                ok(tools::Updated {
                    review_id: p.review_id,
                    status: p.status,
                    seq: event.seq,
                })
            }
            MutatingCall::AddComment(p) => self.add_comment(p).await,
            MutatingCall::Suggest(p) => self.suggest(p).await,
            MutatingCall::Reply(p) => {
                let (comment_id, event) = self
                    .ops_mut()?
                    .reply(p.review_id, p.thread_id, p.body)
                    .await?;
                ok(tools::Replied {
                    comment_id,
                    thread_id: p.thread_id,
                    seq: event.seq,
                })
            }
            MutatingCall::Defer(p) => {
                let event = self
                    .ops_mut()?
                    .mutate(Mutation::DeferThread {
                        review_id: p.review_id,
                        thread_id: p.thread_id,
                        reason: p.reason,
                        tracking_url: p.tracking_url,
                    })
                    .await?;
                ok(tools::Deferred {
                    review_id: p.review_id,
                    thread_id: p.thread_id,
                    seq: event.seq,
                })
            }
            MutatingCall::Resolve(p) => {
                let resolution = if p.resolved {
                    tools::Resolution::Resolved
                } else {
                    tools::Resolution::Open
                };
                let m = match resolution {
                    tools::Resolution::Resolved => Mutation::ResolveThread {
                        review_id: p.review_id,
                        thread_id: p.thread_id,
                    },
                    tools::Resolution::Open => Mutation::UnresolveThread {
                        review_id: p.review_id,
                        thread_id: p.thread_id,
                    },
                };
                let event = self.ops_mut()?.mutate(m).await?;
                ok(tools::Resolved {
                    review_id: p.review_id,
                    thread_id: p.thread_id,
                    resolution,
                    seq: event.seq,
                })
            }
            MutatingCall::RecordCheckpoint(p) => {
                let event = self
                    .ops_mut()?
                    .mutate(Mutation::RecordCheckpoint {
                        review_id: p.review_id,
                        targets: p.targets,
                        in_reply_to: p.in_reply_to,
                    })
                    .await?;
                let checkpoint =
                    nits_protocol::ReviewCheckpoint::from_event(&event).ok_or_else(|| {
                        ToolError::Invalid("daemon returned an unexpected checkpoint event".into())
                    })?;
                ok(checkpoint)
            }
            MutatingCall::RequestReview(p) => {
                let event = self
                    .ops_mut()?
                    .mutate(Mutation::RequestReview {
                        review_id: p.review_id,
                        agent: p.agent.clone(),
                        note: p.note,
                    })
                    .await?;
                ok(tools::Requested {
                    request_id: nits_protocol::ReviewRequestId::from_event_seq(event.seq),
                    review_id: p.review_id,
                    agent: p.agent,
                    seq: event.seq,
                })
            }
        }
    }

    async fn create_review(&mut self, p: tools::CreateReview) -> Result<Value, ToolError> {
        let ops = self.ops_mut()?;
        let implicit = p.workspace_id.is_none() || p.targets.iter().any(|t| t.repo_id.is_none());
        let here = if implicit {
            Some(ops.locate(Path::new(".")).await?)
        } else {
            None
        };
        let workspace_id = p
            .workspace_id
            .or_else(|| here.as_ref().map(|h| h.workspace.id))
            .ok_or_else(|| ToolError::Invalid("workspace_id".into()))?;
        let targets = NonEmpty::new(
            p.targets
                .into_iter()
                .map(|t| {
                    Ok(ReviewTarget {
                        repo_id: t
                            .repo_id
                            .or_else(|| here.as_ref().map(|h| h.repo.id))
                            .ok_or_else(|| ToolError::Invalid("repo_id".into()))?,
                        base: t.base,
                        head: t.head,
                    })
                })
                .collect::<Result<Vec<_>, ToolError>>()?,
        )?;
        let (review_id, event) = ops.create_review(workspace_id, p.title, targets).await?;
        ok(tools::Created {
            review_id,
            seq: event.seq,
        })
    }

    async fn add_comment(&mut self, p: tools::AddComment) -> Result<Value, ToolError> {
        let ops = self.ops_mut()?;
        let anchor = match (p.path, p.start_line) {
            (None, Some(_)) => {
                return Err(ToolError::Invalid("start_line needs a path".into()));
            }
            (None, None) => Anchor::Review,
            (Some(path), start) => {
                let path = RepoPath::new(path)?;
                ops.anchor(
                    p.review_id,
                    p.repo_id,
                    &path,
                    p.side,
                    start.map(|s| (s, p.end_line)),
                )
                .await?
            }
        };
        let (t, event) = ops
            .new_thread(p.review_id, p.intent.into(), anchor, p.body)
            .await?;
        thread_json(t, event.seq)
    }

    async fn suggest(&mut self, p: tools::Suggest) -> Result<Value, ToolError> {
        let ops = self.ops_mut()?;
        let path = RepoPath::new(p.path)?;
        let anchor = ops
            .anchor(
                p.review_id,
                p.repo_id,
                &path,
                p.side,
                Some((p.start_line, p.end_line)),
            )
            .await?;
        let (t, event) = ops
            .new_thread(
                p.review_id,
                CommentKind::Suggestion { patch: p.patch },
                anchor,
                p.body,
            )
            .await?;
        thread_json(t, event.seq)
    }
}

fn thread_json(t: nitsd::ops::NewThread, seq: nits_protocol::Seq) -> Result<Value, ToolError> {
    ok(tools::NewThread {
        comment_id: t.comment_id,
        thread_id: t.thread_id,
        seq,
    })
}

/// Serialise a typed tool result.
fn ok<T: serde::Serialize>(value: T) -> Result<Value, ToolError> {
    Ok(serde_json::to_value(value)?)
}

/// MCP tool result: the value as pretty JSON text plus as structured content.
fn tool_ok(v: &Value) -> Value {
    json!({
        "content": [{ "type": "text", "text": serde_json::to_string_pretty(v).unwrap_or_default() }],
        "structuredContent": v,
    })
}

fn tool_err(e: &ToolError) -> Value {
    json!({
        "content": [{ "type": "text", "text": e.to_string() }],
        "isError": true,
    })
}

async fn finish_dispatch(dispatch: Dispatch) -> Option<Outgoing> {
    match dispatch {
        Dispatch::Reply(reply) => Some(reply),
        Dispatch::Notification | Dispatch::Cancel(_) => None,
        Dispatch::Wait { id, wait } => Some(wait.reply(&id).await),
    }
}

fn tool_reply(id: Value, result: Result<Value, ToolError>) -> Outgoing {
    Outgoing::result(
        id,
        match result {
            Ok(value) => tool_ok(&value),
            Err(error) => tool_err(&error),
        },
    )
}
