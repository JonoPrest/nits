//! The tools: one argument struct and one result struct per tool. Both are
//! serde types with `schemars` derives, so what `tools/list` advertises is
//! exactly what `tools/call` parses and returns — there is no hand-written
//! schema anywhere. Descriptions belong to tool identities, even when
//! argument types are shared.

use nits_config::{ContextKind, ContextName};
use nits_protocol::{
    Author, BaseRefSpec, BlobOid, ChangeKind, Comment, CommentId, Event, FileChange, LineNo,
    LineRange, NonEmpty, RefSpec, RenderContent, RepoId, RepoPath, ResolvedTarget, Review,
    ReviewId, ReviewStatus, Seq, Side, SubscribeScope, Thread, ThreadId, Workspace, WorkspaceId,
};
use schemars::{JsonSchema, Schema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use strum::{Display, EnumDiscriminants, EnumIter, EnumMessage, IntoEnumIterator, IntoStaticStr};

/// A decoded `tools/call`: the variant is the tool, the payload its
/// validated arguments. Adding a tool means adding a variant here; the
/// exhaustive matches in `server.rs` and the table test below then insist on
/// a handler and a `tools/list` entry.
#[derive(Debug, Deserialize, EnumDiscriminants)]
#[strum_discriminants(
    name(ToolName),
    derive(
        EnumIter,
        EnumMessage,
        IntoStaticStr,
        Display,
        Hash,
        PartialOrd,
        Ord,
        Deserialize
    ),
    strum(serialize_all = "snake_case"),
    serde(rename_all = "snake_case")
)]
#[serde(tag = "name", content = "arguments", rename_all = "snake_case")]
pub enum ToolCall {
    #[strum_discriminants(strum(
        message = "List configured contexts, the active daemon and the persisted default. Reads the current config file; does not connect or switch."
    ))]
    ListContexts(NoArgs),
    #[strum_discriminants(strum(
        message = "Switch the active daemon for subsequent calls in this MCP session. Calls run in order; wait for this result before using IDs from that daemon. Connects and negotiates before replacement; failure retains the previous connection. Preserves agent identity, discards old subscriptions, and does not change the persisted default. After switching, pass since_context with any since_seq; cursors and review/workspace IDs belong to their source context."
    ))]
    UseContext(UseContext),
    #[strum_discriminants(strum(
        message = "Workspaces known to the daemon, each with its attached repos."
    ))]
    ListWorkspaces(NoArgs),
    #[strum_discriminants(strum(
        message = "Reviews in a workspace. Without `workspace_id`: the workspace whose attached repo contains this server's working directory."
    ))]
    ListReviews(ListReviews),
    #[strum_discriminants(strum(
        message = "A review with its resolved targets, changed files, threads and comments."
    ))]
    GetReview(ByReview),
    #[strum_discriminants(strum(
        message = "Ensure an open review for a checkout, attaching it on first use. Path is interpreted on the daemon's machine (absolute paths are recommended). Default head is `WorkingTree`; omitted base preserves a matching open review or uses the detected base. Explicit refs must match to reuse a review; use `update_review_target` to change an existing review while keeping threads."
    ))]
    EnsureDirectoryReview(EnsureDirectoryReview),
    #[strum_discriminants(strum(
        message = "Change one existing repository target's base or head, preserving the review, threads and history. The daemon validates refs and reanchors comments."
    ))]
    UpdateReviewTarget(UpdateReviewTarget),
    #[strum_discriminants(strum(
        message = "Create a review over one or more repos. Returns its ID and committed sequence; use `get_review` for its contents and resolved targets. Without `workspace_id`: the workspace containing this server's working directory."
    ))]
    CreateReview(CreateReview),
    #[strum_discriminants(strum(message = "Rename a review or change its status."))]
    UpdateReview(UpdateReview),
    #[strum_discriminants(strum(
        message = "The diff of one changed file in a review, as numbered text (old-line new-line mark text)."
    ))]
    GetDiff(GetDiff),
    #[strum_discriminants(strum(
        message = "Contents of a file at the review's base or head, with absolute line numbers. Works for unchanged files too. Omit both bounds for the full file; otherwise supply both for an inclusive range. Ends beyond EOF are clamped; starts beyond EOF return empty text and a null `returned_range`. Empty files have zero lines. Binary files have no line metadata and reject bounded reads."
    ))]
    GetFile(GetFile),
    #[strum_discriminants(strum(
        message = "Comment threads on a review, including resolution state, comments and attribution."
    ))]
    ListComments(ByReview),
    #[strum_discriminants(strum(
        message = "Start a thread. Anchor to the whole review (no path), a file (path only) or a line range (path + `start_line` [+ `end_line`]) on the given side."
    ))]
    AddComment(AddComment),
    #[strum_discriminants(strum(
        message = "Start a thread carrying a suggested change: a unified diff against the anchored blob that a human can apply."
    ))]
    Suggest(Suggest),
    #[strum_discriminants(strum(message = "Reply in an existing thread."))]
    Reply(Reply),
    #[strum_discriminants(strum(
        message = "Mark a thread resolved (or reopen it with `resolved: false`)."
    ))]
    Resolve(Resolve),
    #[strum_discriminants(strum(
        message = "Ask a named agent to review. Subscribers with scope `AwaitingAgent` for that name are notified. Use the recipient's `get_session_identity` `author.name` unchanged as `agent`."
    ))]
    RequestReview(RequestReview),
    #[strum_discriminants(strum(
        message = "Long-poll for events. Returns events matching the scope after `since_seq`, waiting up to `timeout_ms` for at least one. Pass the returned `last_seq` back as `since_seq` to continue. Mutation results also return a `seq`: use it as `since_seq` for later events, or use an earlier cursor to include the mutation's full event. `review_id`, `workspace_id`, and `awaiting_agent` are mutually exclusive: provide at most one non-null scope filter, or omit all for every event. Cursors are scoped to the returned context.name. After use_context, since_context is required with since_seq; mismatched contexts are rejected."
    ))]
    SubscribeEvents(SubscribeEvents),
    #[strum_discriminants(strum(
        message = "Read this MCP session's current author, including its display/routing name, model and immutable provenance. Use `author.name` for `request_review.agent` and `subscribe_events.awaiting_agent`. Available after initialize, even if the daemon connection is down."
    ))]
    GetSessionIdentity(GetSessionIdentity),
    #[strum_discriminants(strum(
        message = "Set this MCP session's name and model for subsequent events. Both fields are required; use `get_session_identity` to retain a current value. Names and models must be nonempty, with no surrounding whitespace or control characters. The name is also the exact routing key for `request_review.agent` and `subscribe_events.awaiting_agent`; keep it stable while collaborating. Reconnects to the daemon before applying; on failure the old identity remains. Session ID, invoking human, Agent/Mcp provenance and historical authors are preserved. This changes only the calling session and appends no event."
    ))]
    SetSessionIdentity(SetSessionIdentity),
}

/// A call that only reads.
#[derive(Debug)]
pub enum QueryCall {
    ListWorkspaces,
    ListReviews(ListReviews),
    GetReview(ByReview),
    GetDiff(GetDiff),
    GetFile(GetFile),
    ListComments(ByReview),
    SubscribeEvents(SubscribeEvents),
}

/// A call that appends to the log.
#[derive(Debug)]
pub enum MutatingCall {
    EnsureDirectoryReview(EnsureDirectoryReview),
    UpdateReviewTarget(UpdateReviewTarget),
    CreateReview(CreateReview),
    UpdateReview(UpdateReview),
    AddComment(AddComment),
    Suggest(Suggest),
    Reply(Reply),
    Resolve(Resolve),
    RequestReview(RequestReview),
}

/// Session-local operations, separate from daemon queries and event writes.
#[derive(Debug)]
pub enum ContextCall {
    List,
    Use(UseContext),
}

/// Session-local operations, separate from daemon queries and event writes.
#[derive(Debug)]
pub enum SessionCall {
    GetIdentity,
    SetIdentity(SetSessionIdentity),
}

/// A `ToolCall` sorted by the state it accesses, rather than a name list.
#[derive(Debug)]
pub enum Call {
    Query(QueryCall),
    Mutating(MutatingCall),
    Session(SessionCall),
    Context(ContextCall),
}

impl ToolCall {
    /// Decode a tool's arguments. `name` was already parsed, so the only
    /// failures are argument shape errors.
    pub fn parse(name: ToolName, arguments: Value) -> Result<Self, serde_json::Error> {
        let name: &'static str = name.into();
        let mut call = serde_json::Map::new();
        call.insert("name".into(), Value::String(name.into()));
        call.insert("arguments".into(), arguments);
        serde_json::from_value(Value::Object(call))
    }

    #[must_use]
    pub fn name(&self) -> ToolName {
        ToolName::from(self)
    }

    #[must_use]
    pub fn classify(self) -> Call {
        match self {
            ToolCall::ListContexts(NoArgs {}) => Call::Context(ContextCall::List),
            ToolCall::UseContext(p) => Call::Context(ContextCall::Use(p)),
            ToolCall::ListWorkspaces(NoArgs {}) => Call::Query(QueryCall::ListWorkspaces),
            ToolCall::ListReviews(p) => Call::Query(QueryCall::ListReviews(p)),
            ToolCall::GetReview(p) => Call::Query(QueryCall::GetReview(p)),
            ToolCall::GetDiff(p) => Call::Query(QueryCall::GetDiff(p)),
            ToolCall::GetFile(p) => Call::Query(QueryCall::GetFile(p)),
            ToolCall::ListComments(p) => Call::Query(QueryCall::ListComments(p)),
            ToolCall::SubscribeEvents(p) => Call::Query(QueryCall::SubscribeEvents(p)),
            ToolCall::EnsureDirectoryReview(p) => {
                Call::Mutating(MutatingCall::EnsureDirectoryReview(p))
            }
            ToolCall::UpdateReviewTarget(p) => Call::Mutating(MutatingCall::UpdateReviewTarget(p)),
            ToolCall::CreateReview(p) => Call::Mutating(MutatingCall::CreateReview(p)),
            ToolCall::UpdateReview(p) => Call::Mutating(MutatingCall::UpdateReview(p)),
            ToolCall::AddComment(p) => Call::Mutating(MutatingCall::AddComment(p)),
            ToolCall::Suggest(p) => Call::Mutating(MutatingCall::Suggest(p)),
            ToolCall::Reply(p) => Call::Mutating(MutatingCall::Reply(p)),
            ToolCall::Resolve(p) => Call::Mutating(MutatingCall::Resolve(p)),
            ToolCall::RequestReview(p) => Call::Mutating(MutatingCall::RequestReview(p)),
            ToolCall::GetSessionIdentity(GetSessionIdentity {}) => {
                Call::Session(SessionCall::GetIdentity)
            }
            ToolCall::SetSessionIdentity(p) => Call::Session(SessionCall::SetIdentity(p)),
        }
    }
}

/// Arguments of a tool that takes none. Rejects stray keys.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoArgs {}

/// One advertised tool, derived from its argument and result types.
#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    pub name: ToolName,
    /// The tool identity's description.
    pub description: String,
    pub input_schema: Schema,
    pub output_schema: Schema,
}

impl ToolName {
    /// Schemas of this tool's arguments and result. The match is exhaustive,
    /// so a new `ToolCall` variant cannot ship without both.
    #[must_use]
    pub fn schemas(self) -> (Schema, Schema) {
        match self {
            ToolName::ListContexts => (schema_for!(NoArgs), schema_for!(Contexts)),
            ToolName::UseContext => (schema_for!(UseContext), schema_for!(ContextSelected)),
            ToolName::ListWorkspaces => (schema_for!(NoArgs), schema_for!(Workspaces)),
            ToolName::ListReviews => (schema_for!(ListReviews), schema_for!(Reviews)),
            ToolName::GetReview => (schema_for!(ByReview), schema_for!(ReviewDetail)),
            ToolName::EnsureDirectoryReview => (
                schema_for!(EnsureDirectoryReview),
                schema_for!(nits_protocol::DirectoryReview),
            ),
            ToolName::UpdateReviewTarget => {
                (schema_for!(UpdateReviewTarget), schema_for!(TargetUpdated))
            }
            ToolName::CreateReview => (schema_for!(CreateReview), schema_for!(Created)),
            ToolName::UpdateReview => (schema_for!(UpdateReview), schema_for!(Updated)),
            ToolName::GetDiff => (schema_for!(GetDiff), schema_for!(DiffText)),
            ToolName::GetFile => (schema_for!(GetFile), schema_for!(FileText)),
            ToolName::ListComments => (schema_for!(ByReview), schema_for!(Comments)),
            ToolName::AddComment => (schema_for!(AddComment), schema_for!(NewThread)),
            ToolName::Suggest => (schema_for!(Suggest), schema_for!(NewThread)),
            ToolName::Reply => (schema_for!(Reply), schema_for!(Replied)),
            ToolName::Resolve => (schema_for!(Resolve), schema_for!(Resolved)),
            ToolName::RequestReview => (schema_for!(RequestReview), schema_for!(Requested)),
            ToolName::SubscribeEvents => (schema_for!(SubscribeEvents), schema_for!(Events)),
            ToolName::GetSessionIdentity => (
                schema_for!(GetSessionIdentity),
                schema_for!(SessionIdentity),
            ),
            ToolName::SetSessionIdentity => (
                schema_for!(SetSessionIdentity),
                schema_for!(SessionIdentity),
            ),
        }
    }

    #[must_use]
    pub fn tool(self) -> Tool {
        let (mut input_schema, output_schema) = self.schemas();
        let description = self.get_message().unwrap_or_default().to_owned();
        input_schema.insert("title".into(), self.to_string().into());
        input_schema.insert("description".into(), description.clone().into());
        Tool {
            name: self,
            description,
            input_schema,
            output_schema,
        }
    }
}

/// Every tool, in `ToolName` order.
#[must_use]
pub fn all() -> Vec<Tool> {
    ToolName::iter().map(ToolName::tool).collect()
}

/// Select a configured daemon for subsequent calls in this MCP session.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UseContext {
    pub name: ContextName,
}

/// The selected daemon's configured name and transport kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ContextIdentity {
    pub name: ContextName,
    pub kind: ContextKind,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ContextSelected {
    pub context: ContextIdentity,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Contexts {
    pub contexts: Vec<ContextIdentity>,
    pub active: ContextIdentity,
    pub persisted: Option<ContextName>,
}

/// Ensure an open review for a checkout, attaching it on first use. Path is
/// interpreted on the daemon's machine (absolute paths are recommended).
/// Default head is `WorkingTree`; omitted base preserves a matching open review
/// or uses the detected base. Explicit refs must match to reuse a review;
/// use `update_review_target` to change an existing review while keeping threads.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EnsureDirectoryReview {
    pub path: String,
    pub base: Option<BaseRefSpec>,
    pub head: Option<RefSpec>,
}

/// Change one existing repository target's base or head, preserving the review,
/// threads and history. The daemon validates refs and reanchors comments.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateReviewTarget {
    pub review_id: ReviewId,
    pub repo_id: RepoId,
    pub revision: nits_protocol::TargetRevision,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TargetUpdated {
    pub review_id: ReviewId,
    pub repo_id: RepoId,
    /// Committed mutation sequence; subscribe after it for reanchoring events.
    pub seq: Seq,
}

// ---- arguments -------------------------------------------------------------

/// Read this MCP session's current author, including its display/routing name,
/// model and immutable provenance. Use `author.name` for `request_review.agent`
/// and `subscribe_events.awaiting_agent`. Available after initialize, even if
/// the daemon connection is down.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetSessionIdentity {}

/// Set this MCP session's name and model for subsequent events. Both fields
/// are required; use `get_session_identity` to retain a current value. Names and
/// models must be nonempty, with no surrounding whitespace or control characters.
/// The name is also the exact routing key for `request_review.agent` and
/// `subscribe_events.awaiting_agent`; keep it stable while collaborating.
/// Reconnects to the daemon before applying; on failure the old identity remains.
/// Session ID, invoking human, Agent/Mcp provenance and historical authors are
/// preserved. This changes only the calling session and appends no event.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(try_from = "SessionIdentityFields")]
pub struct SetSessionIdentity {
    pub(crate) name: String,
    pub(crate) model: String,
}

/// Editable session fields, validated once at the tool boundary.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SessionIdentityFields {
    /// Display name and exact routing key; nonempty, no surrounding whitespace
    /// or control characters. Choose a distinct name for each collaborating agent.
    #[schemars(length(min = 1))]
    name: String,
    /// Model running this session; nonempty, no surrounding whitespace or
    /// control characters.
    #[schemars(length(min = 1))]
    model: String,
}

impl TryFrom<SessionIdentityFields> for SetSessionIdentity {
    type Error = &'static str;

    fn try_from(fields: SessionIdentityFields) -> Result<Self, Self::Error> {
        for value in [&fields.name, &fields.model] {
            if value.is_empty() || value.trim() != value || value.chars().any(char::is_control) {
                return Err(
                    "name and model must be nonempty, with no surrounding whitespace or control characters",
                );
            }
        }
        Ok(Self {
            name: fields.name,
            model: fields.model,
        })
    }
}

/// Workspaces known to the daemon, each with its attached repos.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListWorkspaces {}

/// Reviews in a workspace. Without `workspace_id`: the workspace whose
/// attached repo contains this server's working directory.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListReviews {
    #[serde(default)]
    pub workspace_id: Option<WorkspaceId>,
}

/// A review with its resolved targets, changed files, threads and comments.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ByReview {
    pub review_id: ReviewId,
}

/// One repo's base and head. `repo_id` may be omitted to mean the repo
/// containing this server's working directory.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TargetSpec {
    #[serde(default)]
    pub repo_id: Option<RepoId>,
    pub base: RefSpec,
    pub head: RefSpec,
}

/// Create a review over one or more repos. Returns its ID and committed
/// sequence; use `get_review` for its contents and resolved targets. Without
/// `workspace_id`: the workspace containing this server's working directory.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateReview {
    #[serde(default)]
    pub workspace_id: Option<WorkspaceId>,
    pub title: String,
    pub targets: Vec<TargetSpec>,
}

/// Rename a review or change its status.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateReview {
    pub review_id: ReviewId,
    pub title: String,
    pub status: ReviewStatus,
}

/// The diff of one changed file in a review, as numbered text
/// (old-line new-line mark text).
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetDiff {
    pub review_id: ReviewId,
    /// Needed only when the review spans several repos.
    pub repo_id: Option<RepoId>,
    /// Path relative to the repo root.
    pub path: String,
    #[serde(default)]
    pub ignore_whitespace: bool,
    /// Lines of context around each hunk; default 3.
    pub context_lines: Option<u32>,
}

/// Contents of a file at the review's base or head, with absolute line numbers.
/// Works for unchanged files too. Omit both bounds for the full file; otherwise
/// supply both for an inclusive range. Ends beyond EOF are clamped; starts beyond
/// EOF return empty text and a null `returned_range`. Empty files have zero lines.
/// Binary files have no line metadata and reject bounded reads.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(try_from = "GetFileWire")]
pub struct GetFile {
    pub review_id: ReviewId,
    pub repo_id: Option<RepoId>,
    pub path: RepoPath,
    pub side: Side,
    pub lines: Option<LineRange>,
}

/// Flat wire bounds become a single validated range before dispatch.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetFileWire {
    review_id: ReviewId,
    /// Needed only when the review spans several repos.
    repo_id: Option<RepoId>,
    /// Path relative to the repo root.
    path: RepoPath,
    /// Default `Head`.
    #[serde(default = "head")]
    side: Side,
    /// Inclusive 1-based start; requires `end_line`. Omit both for the full file.
    start_line: Option<LineNo>,
    /// Inclusive 1-based end, at least `start_line`; clamped at EOF. Requires `start_line`.
    end_line: Option<LineNo>,
}

impl TryFrom<GetFileWire> for GetFile {
    type Error = nitsd::ops::OpsError;

    fn try_from(wire: GetFileWire) -> Result<Self, Self::Error> {
        let lines = match (wire.start_line, wire.end_line) {
            (None, None) => None,
            (Some(start), Some(end)) => Some(LineRange::new(start, end)?),
            (Some(_), None) | (None, Some(_)) => {
                return Err(Self::Error::Invalid(
                    "start_line and end_line must be supplied together".into(),
                ));
            }
        };
        Ok(Self {
            review_id: wire.review_id,
            repo_id: wire.repo_id,
            path: wire.path,
            side: wire.side,
            lines,
        })
    }
}

fn head() -> Side {
    Side::Head
}

/// Start a thread. Anchor to the whole review (no path), a file (path only)
/// or a line range (path + `start_line` [+ `end_line`]) on the given side.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddComment {
    pub review_id: ReviewId,
    /// Needed only when the review spans several repos.
    pub repo_id: Option<RepoId>,
    /// Path relative to the repo root.
    pub path: Option<String>,
    /// Default `Head`.
    #[serde(default = "head")]
    pub side: Side,
    /// 1-based.
    pub start_line: Option<u32>,
    /// Defaults to `start_line`.
    pub end_line: Option<u32>,
    pub body: String,
}

/// Start a thread carrying a suggested change: a unified diff against the
/// anchored blob that a human can apply.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Suggest {
    pub review_id: ReviewId,
    /// Needed only when the review spans several repos.
    pub repo_id: Option<RepoId>,
    /// Path relative to the repo root.
    pub path: String,
    /// Default `Head`.
    #[serde(default = "head")]
    pub side: Side,
    /// 1-based.
    pub start_line: u32,
    /// Defaults to `start_line`.
    pub end_line: Option<u32>,
    /// Unified diff against the file at that side.
    pub patch: String,
    /// Why.
    pub body: String,
}

/// Reply in an existing thread.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Reply {
    pub review_id: ReviewId,
    pub thread_id: ThreadId,
    pub body: String,
}

/// Mark a thread resolved (or reopen it with `resolved: false`).
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Resolve {
    pub review_id: ReviewId,
    pub thread_id: ThreadId,
    /// Default `true`.
    #[serde(default = "yes")]
    pub resolved: bool,
}

fn yes() -> bool {
    true
}

/// Ask a named agent to review. Subscribers with scope `AwaitingAgent` for
/// that name are notified. Use the recipient's `get_session_identity` `author.name`
/// unchanged as `agent`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestReview {
    pub review_id: ReviewId,
    pub agent: String,
    pub note: String,
}

/// Long-poll for events. Returns events matching the scope after
/// `since_seq`, waiting up to `timeout_ms` for at least one. Pass the
/// returned `last_seq` back as `since_seq` to continue.
/// Mutation results also return a `seq`: use it as `since_seq` for later
/// events, or use an earlier cursor to include the mutation's full event.
/// `review_id`, `workspace_id`, and `awaiting_agent` are mutually exclusive:
/// provide at most one non-null scope filter, or omit all for every event.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(try_from = "SubscribeEventsWire")]
#[schemars(with = "SubscribeEventsWire")]
pub struct SubscribeEvents {
    pub scope: SubscribeScope,
    pub start: EventStart,
    pub timeout_ms: u64,
    pub max: usize,
}

/// A subscription starts live or replays a position from one source.
#[derive(Debug, PartialEq, Eq)]
pub enum EventStart {
    Live,
    Replay { seq: Seq, source: CursorSource },
}

/// Bare cursors remain accepted until the MCP session switches contexts.
#[derive(Debug, PartialEq, Eq)]
pub enum CursorSource {
    InitialContext,
    Named(ContextName),
}

/// Flat MCP arguments, converted to a single scope at the serde boundary.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SubscribeEventsWire {
    /// Only events for this review. Mutually exclusive with `workspace_id`
    /// and `awaiting_agent`.
    review_id: Option<ReviewId>,
    /// Only events for this workspace. Mutually exclusive with `review_id`
    /// and `awaiting_agent`.
    workspace_id: Option<WorkspaceId>,
    /// Only `ReviewRequested` events addressed to this agent name. Mutually
    /// exclusive with `review_id` and `workspace_id`. To receive this session's
    /// requests, use `get_session_identity`'s `author.name` unchanged.
    awaiting_agent: Option<String>,
    /// Replay after this log position; omit for live only.
    since_seq: Option<Seq>,
    /// Context name that issued `since_seq`. Required after `use_context`;
    /// a name different from the active context is rejected.
    since_context: Option<ContextName>,
    /// Default 30000.
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
    /// Default 100.
    #[serde(default = "default_max")]
    max: usize,
}

#[derive(Debug, thiserror::Error)]
enum InvalidSubscribe {
    #[error(
        "review_id, workspace_id, and awaiting_agent are mutually exclusive; provide at most one non-null scope filter, or omit all for every event"
    )]
    ConflictingScopes,
    #[error("since_context requires since_seq")]
    ContextWithoutSequence,
}

impl TryFrom<SubscribeEventsWire> for SubscribeEvents {
    type Error = InvalidSubscribe;

    fn try_from(wire: SubscribeEventsWire) -> Result<Self, Self::Error> {
        let scope = match (wire.review_id, wire.workspace_id, wire.awaiting_agent) {
            (None, None, None) => SubscribeScope::All,
            (Some(review_id), None, None) => SubscribeScope::Review { review_id },
            (None, Some(workspace_id), None) => SubscribeScope::Workspace { workspace_id },
            (None, None, Some(agent)) => SubscribeScope::AwaitingAgent { agent },
            (Some(_), Some(_), _) | (Some(_), None, Some(_)) | (None, Some(_), Some(_)) => {
                return Err(InvalidSubscribe::ConflictingScopes);
            }
        };
        let start = match (wire.since_seq, wire.since_context) {
            (None, None) => EventStart::Live,
            (None, Some(_)) => return Err(InvalidSubscribe::ContextWithoutSequence),
            (Some(seq), source) => EventStart::Replay {
                seq,
                source: source.map_or(CursorSource::InitialContext, CursorSource::Named),
            },
        };
        Ok(Self {
            scope,
            start,
            timeout_ms: wire.timeout_ms,
            max: wire.max,
        })
    }
}

fn default_timeout() -> u64 {
    30_000
}
fn default_max() -> usize {
    100
}

// ---- results ---------------------------------------------------------------

/// The current author for this MCP session's future events. Its Agent name is
/// also the exact routing key; `session_id`, `invoked_by` and `via` are read-only.
#[derive(Debug, Serialize, JsonSchema)]
pub struct SessionIdentity {
    pub author: Author,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Workspaces {
    pub context: ContextIdentity,
    pub workspaces: Vec<Workspace>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Reviews {
    pub context: ContextIdentity,
    pub reviews: Vec<Review>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ReviewDetail {
    pub context: ContextIdentity,
    pub review: Review,
    pub resolved: Option<NonEmpty<ResolvedTarget>>,
    pub files: Vec<FileChange>,
    pub threads: Vec<Thread>,
    pub comments: Vec<Comment>,
    /// Log position this state reflects.
    pub seq: Seq,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Created {
    pub review_id: ReviewId,
    /// Committed mutation sequence; pass as `subscribe_events.since_seq` for later events.
    pub seq: Seq,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Updated {
    pub review_id: ReviewId,
    pub status: ReviewStatus,
    /// Committed mutation sequence; pass as `subscribe_events.since_seq` for later events.
    pub seq: Seq,
}

/// Thread state acknowledged by `resolve`, without event provenance.
#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
pub enum Resolution {
    Open,
    Resolved,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Resolved {
    pub review_id: ReviewId,
    pub thread_id: ThreadId,
    pub resolution: Resolution,
    /// Committed mutation sequence; pass as `subscribe_events.since_seq` for later events.
    pub seq: Seq,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Requested {
    pub review_id: ReviewId,
    pub agent: String,
    /// Committed mutation sequence; pass as `subscribe_events.since_seq` for later events.
    pub seq: Seq,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DiffText {
    pub context: ContextIdentity,
    pub repo_id: RepoId,
    pub path: RepoPath,
    pub change: ChangeKind,
    pub lang: Option<String>,
    pub content: RenderContent,
    /// Numbered diff text: old-line, new-line, mark, text.
    pub text: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct FileText {
    pub context: ContextIdentity,
    pub repo_id: RepoId,
    pub path: RepoPath,
    pub side: Side,
    pub blob_oid: BlobOid,
    pub lang: Option<String>,
    pub content: RenderContent,
    /// Source-line metadata; null for binary files. Render content describes the full blob.
    pub lines: Option<FileLines>,
    /// Numbered file text.
    pub text: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct FileLines {
    /// Number of source lines in the full blob. A trailing newline adds no extra line.
    pub total_lines: u32,
    /// Actual inclusive source range returned; null for empty files or starts beyond EOF.
    pub returned_range: Option<LineRange>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Comments {
    pub context: ContextIdentity,
    pub threads: Vec<Thread>,
    pub comments: Vec<Comment>,
    pub seq: Seq,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct NewThread {
    pub comment_id: CommentId,
    pub thread_id: ThreadId,
    /// Committed mutation sequence; pass as `subscribe_events.since_seq` for later events.
    pub seq: Seq,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Replied {
    pub comment_id: CommentId,
    pub thread_id: ThreadId,
    /// Committed mutation sequence; pass as `subscribe_events.since_seq` for later events.
    pub seq: Seq,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Events {
    pub context: ContextIdentity,
    pub events: Vec<Event>,
    /// Pass back as `since_seq`.
    pub last_seq: Seq,
}

#[cfg(test)]
mod tests {
    use strum::IntoEnumIterator;

    use super::*;

    #[test]
    fn every_tool_is_advertised_once_in_order() {
        let advertised: Vec<ToolName> = all().into_iter().map(|t| t.name).collect();
        let expected: Vec<ToolName> = ToolName::iter().collect();
        assert_eq!(advertised, expected);
    }

    #[test]
    fn every_tool_has_a_description_and_object_schemas() {
        for t in all() {
            assert!(!t.description.is_empty(), "{} has no doc comment", t.name);
            for s in [&t.input_schema, &t.output_schema] {
                assert_eq!(
                    s.get("type").and_then(Value::as_str),
                    Some("object"),
                    "{}",
                    t.name
                );
            }
        }
        let (input, _) = ToolName::GetFile.schemas();
        let props = input.get("properties").unwrap();
        assert!(props.get("side").is_some());
        assert_eq!(
            input.get("required"),
            Some(&serde_json::json!(["review_id", "path"]))
        );
    }

    #[test]
    fn subscription_cursors_parse_the_source_with_the_position() {
        assert!(
            ToolCall::parse(
                ToolName::SubscribeEvents,
                serde_json::json!({ "since_context": "remote" })
            )
            .is_err()
        );
        let call = ToolCall::parse(
            ToolName::SubscribeEvents,
            serde_json::json!({ "since_context": "remote", "since_seq": 7 }),
        )
        .unwrap();
        let Call::Query(QueryCall::SubscribeEvents(parsed)) = call.classify() else {
            panic!("expected subscription");
        };
        assert_eq!(
            parsed.start,
            EventStart::Replay {
                seq: Seq::new(7),
                source: CursorSource::Named("remote".parse().unwrap())
            }
        );
    }

    #[test]
    fn shared_arguments_have_tool_specific_descriptions_and_titles() {
        let review = ToolName::GetReview.tool();
        let comments = ToolName::ListComments.tool();
        assert_ne!(review.description, comments.description);
        assert!(comments.description.contains("Comment threads"));
        assert!(review.description.contains("resolved targets"));
        assert_eq!(
            review.input_schema.get("title"),
            Some(&Value::String("get_review".into()))
        );
        assert_eq!(
            comments.input_schema.get("title"),
            Some(&Value::String("list_comments".into()))
        );
        assert!(ToolCall::parse(ToolName::UseContext, serde_json::json!({ "name": " " })).is_err());
        assert!(matches!(
            ToolCall::parse(
                ToolName::UseContext,
                serde_json::json!({ "name": "remote" })
            )
            .unwrap()
            .classify(),
            Call::Context(ContextCall::Use(_))
        ));
    }

    #[test]
    fn bootstrap_advertises_working_tree_only_for_head() {
        let (input, _) = ToolName::EnsureDirectoryReview.schemas();
        let properties = input.get("properties").unwrap();
        assert!(
            properties["base"]
                .to_string()
                .contains("#/$defs/BaseRefSpec")
        );
        assert!(properties["head"].to_string().contains("#/$defs/RefSpec"));
        let definitions = input.get("$defs").unwrap();
        assert!(
            !definitions["BaseRefSpec"]
                .to_string()
                .contains("WorkingTree")
        );
        assert!(definitions["RefSpec"].to_string().contains("WorkingTree"));
    }

    #[test]
    fn mutation_schemas_are_focused_and_only_subscriptions_advertise_events() {
        for tool in all() {
            let expected_fields: Option<&[&str]> = match tool.name {
                ToolName::EnsureDirectoryReview => Some(&[
                    "workspace_id",
                    "repo_id",
                    "review_id",
                    "base",
                    "head",
                    "outcome",
                    "seq",
                ]),
                ToolName::UpdateReviewTarget => Some(&["review_id", "repo_id", "seq"]),
                ToolName::CreateReview => Some(&["review_id", "seq"]),
                ToolName::UpdateReview => Some(&["review_id", "status", "seq"]),
                ToolName::AddComment | ToolName::Suggest | ToolName::Reply => {
                    Some(&["comment_id", "thread_id", "seq"])
                }
                ToolName::Resolve => Some(&["review_id", "thread_id", "resolution", "seq"]),
                ToolName::RequestReview => Some(&["review_id", "agent", "seq"]),
                ToolName::ListContexts
                | ToolName::UseContext
                | ToolName::ListWorkspaces
                | ToolName::ListReviews
                | ToolName::GetReview
                | ToolName::GetDiff
                | ToolName::GetFile
                | ToolName::ListComments
                | ToolName::SubscribeEvents
                | ToolName::GetSessionIdentity
                | ToolName::SetSessionIdentity => None,
            };
            let schema = serde_json::to_string(&tool.output_schema).unwrap();
            assert_eq!(
                schema.contains("EventBody"),
                tool.name == ToolName::SubscribeEvents,
                "{}: heterogeneous events belong only to subscriptions",
                tool.name
            );
            if let Some(fields) = expected_fields {
                let expected: std::collections::BTreeSet<_> = fields.iter().copied().collect();
                let properties = tool
                    .output_schema
                    .get("properties")
                    .unwrap()
                    .as_object()
                    .unwrap();
                let actual = properties.keys().map(String::as_str).collect();
                assert_eq!(expected, actual, "{}", tool.name);
                let required = tool
                    .output_schema
                    .get("required")
                    .unwrap()
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|field| field.as_str().unwrap())
                    .collect();
                assert_eq!(expected, required, "{}", tool.name);
                let size_limit = if tool.name == ToolName::EnsureDirectoryReview {
                    5000
                } else {
                    2500
                };
                assert!(
                    schema.len() < size_limit,
                    "{} output schema grew to {} bytes",
                    tool.name,
                    schema.len()
                );
                assert!(!schema.contains("WorkspaceCreated"), "{}", tool.name);
                assert!(!schema.contains("SuggestionApplied"), "{}", tool.name);
            }
        }
    }

    #[test]
    fn names_round_trip_through_the_wire_form() {
        for name in ToolName::iter() {
            let wire: &'static str = name.into();
            assert_eq!(name.to_string(), wire);
            assert!(wire.chars().all(|c| c.is_ascii_lowercase() || c == '_'));
        }
        let call = ToolCall::parse(ToolName::ListWorkspaces, serde_json::json!({})).unwrap();
        assert_eq!(call.name(), ToolName::ListWorkspaces);
        assert!(ToolCall::parse(ToolName::ListWorkspaces, serde_json::json!({ "x": 1 })).is_err());
        assert!(ToolCall::parse(ToolName::GetReview, serde_json::json!({})).is_err());
    }

    #[test]
    fn file_bounds_parse_to_a_single_range_and_remain_a_query() {
        let review_id = ReviewId::from_parts(1, 1);
        for (bounds, expected) in [
            (serde_json::json!({}), None),
            (
                serde_json::json!({ "start_line": null, "end_line": null }),
                None,
            ),
            (
                serde_json::json!({ "start_line": 995, "end_line": 1015 }),
                Some(
                    LineRange::new(LineNo::new(995).unwrap(), LineNo::new(1015).unwrap()).unwrap(),
                ),
            ),
        ] {
            let mut args = bounds;
            args["review_id"] = serde_json::json!(review_id);
            args["path"] = serde_json::json!("source.rs");
            let Call::Query(QueryCall::GetFile(parsed)) =
                ToolCall::parse(ToolName::GetFile, args).unwrap().classify()
            else {
                panic!("get_file must remain a query");
            };
            assert_eq!(parsed.lines, expected);
            assert_eq!(parsed.side, Side::Head);
        }
        let tool = ToolName::GetFile.tool();
        assert!(tool.description.contains("inclusive range"));
        assert!(tool.description.contains("EOF"));
        assert_eq!(
            tool.input_schema.get("additionalProperties"),
            Some(&Value::Bool(false))
        );
        let properties = tool.input_schema.get("properties").unwrap();
        assert!(properties.get("start_line").is_some());
        assert!(properties.get("end_line").is_some());
        assert!(properties.get("lines").is_none());
        assert!(
            tool.output_schema
                .get("properties")
                .unwrap()
                .get("lines")
                .is_some()
        );
    }

    #[test]
    fn session_identity_arguments_allow_only_valid_name_and_model() {
        let call = ToolCall::parse(
            ToolName::SetSessionIdentity,
            serde_json::json!({ "name": "reviewer-a", "model": "example-model" }),
        )
        .unwrap();
        let Call::Session(SessionCall::SetIdentity(identity)) = call.classify() else {
            panic!("identity changes are session-local");
        };
        assert_eq!(identity.name, "reviewer-a");
        assert_eq!(identity.model, "example-model");
        assert!(matches!(
            ToolCall::parse(ToolName::GetSessionIdentity, serde_json::json!({}))
                .unwrap()
                .classify(),
            Call::Session(SessionCall::GetIdentity)
        ));

        for invalid in ["", " ", " reviewer-a", "reviewer-a ", "a\nb", "a\0b"] {
            for arguments in [
                serde_json::json!({ "name": invalid, "model": "model" }),
                serde_json::json!({ "name": "reviewer-a", "model": invalid }),
            ] {
                assert!(ToolCall::parse(ToolName::SetSessionIdentity, arguments).is_err());
            }
        }
        for arguments in [
            serde_json::json!({}),
            serde_json::json!({ "name": "reviewer-a" }),
            serde_json::json!({ "model": "model" }),
            serde_json::json!({ "name": null, "model": "model" }),
            serde_json::json!({ "name": "reviewer-a", "model": 42 }),
        ] {
            assert!(ToolCall::parse(ToolName::SetSessionIdentity, arguments).is_err());
        }
        for field in [
            "session_id",
            "invoked_by",
            "via",
            "type",
            "author",
            "client_id",
        ] {
            let mut arguments = serde_json::json!({ "name": "reviewer-a", "model": "model" });
            arguments[field] = serde_json::json!("forged");
            assert!(ToolCall::parse(ToolName::SetSessionIdentity, arguments).is_err());
            assert!(
                ToolCall::parse(
                    ToolName::GetSessionIdentity,
                    serde_json::json!({ field: "another session" }),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn identity_schema_and_descriptions_explain_routing_and_editable_fields() {
        let tool = ToolName::SetSessionIdentity.tool();
        assert!(tool.description.contains("subsequent events"));
        assert!(tool.description.contains("request_review.agent"));
        assert!(tool.description.contains("subscribe_events.awaiting_agent"));
        assert_eq!(
            tool.input_schema.get("required"),
            Some(&serde_json::json!(["name", "model"]))
        );
        assert_eq!(
            tool.input_schema.get("additionalProperties"),
            Some(&Value::Bool(false))
        );
        let properties = tool
            .input_schema
            .get("properties")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(properties.len(), 2);
        assert_eq!(properties["name"]["minLength"], 1);
        assert_eq!(properties["model"]["minLength"], 1);
        for name in [ToolName::GetSessionIdentity, ToolName::SetSessionIdentity] {
            assert!(
                name.tool()
                    .output_schema
                    .get("properties")
                    .unwrap()
                    .get("author")
                    .is_some()
            );
        }
    }

    #[test]
    fn subscribe_scopes_are_parsed_once_from_flat_arguments() {
        let review_id = ReviewId::from_parts(1, 1);
        let workspace_id = WorkspaceId::from_parts(1, 1);
        for (arguments, expected) in [
            (serde_json::json!({}), SubscribeScope::All),
            (
                serde_json::json!({ "review_id": null, "workspace_id": null, "awaiting_agent": null }),
                SubscribeScope::All,
            ),
            (
                serde_json::json!({ "review_id": review_id }),
                SubscribeScope::Review { review_id },
            ),
            (
                serde_json::json!({ "workspace_id": workspace_id }),
                SubscribeScope::Workspace { workspace_id },
            ),
            (
                serde_json::json!({ "awaiting_agent": "reviewer-a" }),
                SubscribeScope::AwaitingAgent {
                    agent: "reviewer-a".into(),
                },
            ),
            (
                serde_json::json!({ "review_id": null, "workspace_id": workspace_id, "awaiting_agent": null }),
                SubscribeScope::Workspace { workspace_id },
            ),
        ] {
            let ToolCall::SubscribeEvents(parsed) =
                ToolCall::parse(ToolName::SubscribeEvents, arguments).unwrap()
            else {
                panic!("expected subscribe_events");
            };
            assert_eq!(parsed.scope, expected);
            assert_eq!(parsed.start, EventStart::Live);
            assert_eq!(parsed.timeout_ms, 30_000);
            assert_eq!(parsed.max, 100);
        }

        let parsed: SubscribeEvents = serde_json::from_value(serde_json::json!({
            "review_id": review_id, "since_seq": 7, "timeout_ms": 5, "max": 3
        }))
        .unwrap();
        assert_eq!(parsed.scope, SubscribeScope::Review { review_id });
        assert_eq!(
            parsed.start,
            EventStart::Replay {
                seq: Seq::new(7),
                source: CursorSource::InitialContext
            }
        );
        assert_eq!(parsed.timeout_ms, 5);
        assert_eq!(parsed.max, 3);
    }

    #[test]
    fn subscribe_rejects_every_combination_of_conflicting_scopes() {
        let review_id = ReviewId::from_parts(1, 1);
        let workspace_id = WorkspaceId::from_parts(1, 1);
        for arguments in [
            serde_json::json!({ "review_id": review_id, "workspace_id": workspace_id }),
            serde_json::json!({ "review_id": review_id, "awaiting_agent": "reviewer-a" }),
            serde_json::json!({ "workspace_id": workspace_id, "awaiting_agent": "reviewer-a" }),
            serde_json::json!({ "review_id": review_id, "workspace_id": workspace_id, "awaiting_agent": "reviewer-a" }),
        ] {
            let error = ToolCall::parse(ToolName::SubscribeEvents, arguments)
                .unwrap_err()
                .to_string();
            assert!(
                error
                    .contains("review_id, workspace_id, and awaiting_agent are mutually exclusive"),
                "{error}"
            );
            assert!(
                error.contains("provide at most one non-null scope filter"),
                "{error}"
            );
        }
        for arguments in [
            serde_json::json!({ "scope": { "type": "All" } }),
            serde_json::json!({ "review_id": "invalid" }),
            serde_json::json!({ "workspace_id": "invalid" }),
            serde_json::json!({ "awaiting_agent": 42 }),
        ] {
            assert!(ToolCall::parse(ToolName::SubscribeEvents, arguments).is_err());
        }
    }

    #[test]
    fn subscribe_schema_documents_exclusive_flat_scopes() {
        let tool = ToolName::SubscribeEvents.tool();
        assert!(tool.description.contains("mutually exclusive"));
        assert!(tool.description.contains("omit all for every event"));
        let properties = tool.input_schema.get("properties").unwrap();
        for field in ["review_id", "workspace_id", "awaiting_agent"] {
            assert!(
                properties[field]["description"]
                    .as_str()
                    .unwrap()
                    .contains("exclusive")
            );
        }
        assert!(
            properties.get("scope").is_none(),
            "keep the existing flat wire shape"
        );
        assert_eq!(
            tool.input_schema.get("additionalProperties"),
            Some(&Value::Bool(false))
        );
    }
}
