//! The MCP server: `initialize`, `tools/list`, `tools/call`, proxied to the
//! daemon through [`nitsd::ops::Ops`]. One request at a time, in order — MCP
//! clients pipeline rarely and the daemon connection is shared, so
//! serialising keeps event long-polls from interleaving with other calls.

use std::path::Path;
use std::time::Duration;

use nits_protocol::{
    AgentVia, Anchor, Author, BuildInfo, ClientId, CommentKind, Human, Mutation, NonEmpty,
    RenderOpts, RepoPath, ReviewTarget, Since,
};
use nitsd::client::{Client, ClientError, Identity};
use nitsd::ops::{Ops, OpsError};
use nitsd::render_text as text;
use serde::Deserialize;
use serde_json::{Value, json};
use strum::EnumString;

use crate::jsonrpc::{self, Incoming, Outgoing};
use crate::tools::{self, Call, MutatingCall, QueryCall, SessionCall, ToolCall, ToolName};

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
    pub context: nits_config::Context,
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
}

#[derive(Debug)]
pub struct Server {
    endpoint: Endpoint,
    agent: AgentIdentity,
    build: BuildInfo,
    session: Option<Session>,
}

impl Server {
    #[must_use]
    pub fn new(endpoint: Endpoint, agent: AgentIdentity, build: BuildInfo) -> Self {
        Self {
            endpoint,
            agent,
            build,
            session: None,
        }
    }

    /// The daemon connection, once `initialize` has run.
    #[must_use]
    pub fn client(&self) -> Option<&Client> {
        self.session.as_ref().map(|s| s.ops.client())
    }

    /// Handle one line of stdin. Notifications produce no reply.
    pub async fn handle_line(&mut self, line: &str) -> Option<Outgoing> {
        let msg: Incoming = match serde_json::from_str(line) {
            Ok(m) => m,
            Err(e) => {
                return Some(Outgoing::error(
                    Value::Null,
                    jsonrpc::PARSE_ERROR,
                    format!("parse error: {e}"),
                ));
            }
        };
        self.handle(msg).await
    }

    /// Handle one decoded message.
    pub async fn handle(&mut self, msg: Incoming) -> Option<Outgoing> {
        if msg.jsonrpc != "2.0" {
            return Some(Outgoing::error(
                msg.id.unwrap_or(Value::Null),
                jsonrpc::INVALID_REQUEST,
                "jsonrpc must be \"2.0\"",
            ));
        }
        let id = msg.id?;
        let Ok(method) = msg.method.parse::<Method>() else {
            return Some(Outgoing::error(
                id,
                jsonrpc::METHOD_NOT_FOUND,
                format!("method not found: {}", msg.method),
            ));
        };
        Some(match method {
            Method::Initialize => match self.initialize(&msg.params).await {
                Ok(v) => Outgoing::result(id, v),
                Err(e) => Outgoing::error(id, jsonrpc::INTERNAL_ERROR, e.to_string()),
            },
            Method::Ping => Outgoing::result(id, json!({})),
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
                        return Some(Outgoing::error(
                            id,
                            jsonrpc::INVALID_PARAMS,
                            format!("invalid tools/call params: {e}"),
                        ));
                    }
                };
                let call = match ToolCall::parse(params.name, params.arguments) {
                    Ok(c) => c,
                    Err(e) => return Some(Outgoing::result(id, tool_err(&ToolError::from(e)))),
                };
                match self.call(call).await {
                    Ok(v) => Outgoing::result(id, tool_ok(&v)),
                    Err(e) => Outgoing::result(id, tool_err(&e)),
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
        });
        Ok(json!({
            "protocolVersion": MCP_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": self.build.name, "version": self.build.version },
            "instructions": format!(
                "Nits code review. Connected to {} {}. Start with list_workspaces, then get_review; \
                 anchor comments with add_comment/suggest; wait for work with subscribe_events. \
                 Use get_session_identity to see your author; set_session_identity with name and \
                 model before posting to identify this agent. Share the returned author.name as \
                 the exact request_review.agent / subscribe_events.awaiting_agent routing key \
                 and keep it stable while collaborating. Identity changes affect only future events.",
                daemon.name, daemon.version
            ),
        }))
    }

    async fn connect(&self, author: Author) -> Result<Client, ToolError> {
        let (ts, r) = nitsd::ids::fresh_parts();
        let identity = Identity {
            client_id: ClientId::from_parts(ts, r),
            client: self.build.clone(),
            author,
        };
        tokio::time::timeout(
            CONNECT_TIMEOUT,
            nitsd::contexts::connect(&self.endpoint.context, identity, self.endpoint.start),
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
            });
        }
        Ok(())
    }

    /// Run a decoded tool call. Public so tests can bypass JSON-RPC.
    pub async fn call(&mut self, call: ToolCall) -> Result<Value, ToolError> {
        match call.classify() {
            Call::Query(q) => {
                self.ensure_connected().await?;
                self.call_query(q).await
            }
            Call::Mutating(m) => {
                self.ensure_connected().await?;
                self.call_mutating(m).await
            }
            Call::Session(s) => self.call_session(s).await,
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
                });
                author
            }
        };
        ok(tools::SessionIdentity { author })
    }

    async fn call_query(&self, call: QueryCall) -> Result<Value, ToolError> {
        let ops = self.ops()?;
        match call {
            QueryCall::ListWorkspaces => ok(tools::Workspaces {
                workspaces: ops.workspaces().await?,
            }),
            QueryCall::ListReviews(p) => {
                let workspace_id = match p.workspace_id {
                    Some(w) => w,
                    None => ops.locate(Path::new(".")).await?.workspace.id,
                };
                ok(tools::Reviews {
                    reviews: ops.reviews(workspace_id).await?,
                })
            }
            QueryCall::GetReview(p) => {
                let snap = ops.snapshot(p.review_id).await?;
                let files = ops.files(p.review_id).await?;
                ok(tools::ReviewDetail {
                    review: snap.review,
                    resolved: snap.resolved,
                    files,
                    threads: snap.threads,
                    comments: snap.comments,
                    seq: snap.seq,
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
                    .diff(p.review_id, p.repo_id, &p.path, render_opts)
                    .await?;
                let text = text::render(&header, &chunks);
                ok(tools::DiffText {
                    repo_id: file.repo_id,
                    path: file.path,
                    change: file.kind,
                    lang: header.lang,
                    content: header.content,
                    text,
                })
            }
            QueryCall::GetFile(p) => {
                let path = RepoPath::new(p.path)?;
                let (repo_id, blob_oid, header, chunks) =
                    ops.file_at(p.review_id, p.repo_id, &path, p.side).await?;
                let text = text::render_blob(&header, &chunks);
                ok(tools::FileText {
                    repo_id,
                    path,
                    side: p.side,
                    blob_oid,
                    lang: header.lang,
                    content: header.content,
                    text,
                })
            }
            QueryCall::ListComments(p) => {
                let snap = ops.snapshot(p.review_id).await?;
                ok(tools::Comments {
                    threads: snap.threads,
                    comments: snap.comments,
                    seq: snap.seq,
                })
            }
            QueryCall::SubscribeEvents(p) => {
                let since = p.since_seq.map_or(Since::Now, |seq| Since::After { seq });
                let polled = ops
                    .poll_events(p.scope, since, Duration::from_millis(p.timeout_ms), p.max)
                    .await?;
                ok(tools::Events {
                    events: polled.events,
                    last_seq: polled.last_seq,
                })
            }
        }
    }

    async fn call_mutating(&mut self, call: MutatingCall) -> Result<Value, ToolError> {
        match call {
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
            .new_thread(p.review_id, CommentKind::Note, anchor, p.body)
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
