//! The tools: one argument struct and one result struct per tool. Both are
//! serde types with `schemars` derives, so what `tools/list` advertises is
//! exactly what `tools/call` parses and returns — there is no hand-written
//! schema anywhere. The tool's description is the argument struct's doc
//! comment.

use nits_protocol::{
    BlobOid, ChangeKind, Comment, CommentId, Event, FileChange, NonEmpty, RefSpec, RenderContent,
    RepoId, RepoPath, ResolvedTarget, Review, ReviewId, ReviewStatus, Seq, Side, SubscribeScope,
    Thread, ThreadId, Workspace, WorkspaceId,
};
use schemars::{JsonSchema, Schema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use strum::{Display, EnumDiscriminants, EnumIter, IntoEnumIterator, IntoStaticStr};

/// A decoded `tools/call`: the variant is the tool, the payload its
/// validated arguments. Adding a tool means adding a variant here; the
/// exhaustive matches in `server.rs` and the table test below then insist on
/// a handler and a `tools/list` entry.
#[derive(Debug, Deserialize, EnumDiscriminants)]
#[strum_discriminants(
    name(ToolName),
    derive(EnumIter, IntoStaticStr, Display, Hash, PartialOrd, Ord, Deserialize),
    strum(serialize_all = "snake_case"),
    serde(rename_all = "snake_case")
)]
#[serde(tag = "name", content = "arguments", rename_all = "snake_case")]
pub enum ToolCall {
    ListWorkspaces(NoArgs),
    ListReviews(ListReviews),
    GetReview(ByReview),
    CreateReview(CreateReview),
    UpdateReview(UpdateReview),
    GetDiff(GetDiff),
    GetFile(GetFile),
    ListComments(ByReview),
    AddComment(AddComment),
    Suggest(Suggest),
    Reply(Reply),
    Resolve(Resolve),
    RequestReview(RequestReview),
    SubscribeEvents(SubscribeEvents),
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
    CreateReview(CreateReview),
    UpdateReview(UpdateReview),
    AddComment(AddComment),
    Suggest(Suggest),
    Reply(Reply),
    Resolve(Resolve),
    RequestReview(RequestReview),
}

/// A `ToolCall` sorted by whether it mutates, so the read/write split is a
/// type rather than a name list.
#[derive(Debug)]
pub enum Call {
    Query(QueryCall),
    Mutating(MutatingCall),
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
            ToolCall::ListWorkspaces(NoArgs {}) => Call::Query(QueryCall::ListWorkspaces),
            ToolCall::ListReviews(p) => Call::Query(QueryCall::ListReviews(p)),
            ToolCall::GetReview(p) => Call::Query(QueryCall::GetReview(p)),
            ToolCall::GetDiff(p) => Call::Query(QueryCall::GetDiff(p)),
            ToolCall::GetFile(p) => Call::Query(QueryCall::GetFile(p)),
            ToolCall::ListComments(p) => Call::Query(QueryCall::ListComments(p)),
            ToolCall::SubscribeEvents(p) => Call::Query(QueryCall::SubscribeEvents(p)),
            ToolCall::CreateReview(p) => Call::Mutating(MutatingCall::CreateReview(p)),
            ToolCall::UpdateReview(p) => Call::Mutating(MutatingCall::UpdateReview(p)),
            ToolCall::AddComment(p) => Call::Mutating(MutatingCall::AddComment(p)),
            ToolCall::Suggest(p) => Call::Mutating(MutatingCall::Suggest(p)),
            ToolCall::Reply(p) => Call::Mutating(MutatingCall::Reply(p)),
            ToolCall::Resolve(p) => Call::Mutating(MutatingCall::Resolve(p)),
            ToolCall::RequestReview(p) => Call::Mutating(MutatingCall::RequestReview(p)),
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
    /// The argument struct's doc comment.
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
            ToolName::ListWorkspaces => (schema_for!(NoArgs), schema_for!(Workspaces)),
            ToolName::ListReviews => (schema_for!(ListReviews), schema_for!(Reviews)),
            ToolName::GetReview => (schema_for!(ByReview), schema_for!(ReviewDetail)),
            ToolName::CreateReview => (schema_for!(CreateReview), schema_for!(Created)),
            ToolName::UpdateReview => (schema_for!(UpdateReview), schema_for!(Committed)),
            ToolName::GetDiff => (schema_for!(GetDiff), schema_for!(DiffText)),
            ToolName::GetFile => (schema_for!(GetFile), schema_for!(FileText)),
            ToolName::ListComments => (schema_for!(ByReview), schema_for!(Comments)),
            ToolName::AddComment => (schema_for!(AddComment), schema_for!(NewThread)),
            ToolName::Suggest => (schema_for!(Suggest), schema_for!(NewThread)),
            ToolName::Reply => (schema_for!(Reply), schema_for!(Replied)),
            ToolName::Resolve => (schema_for!(Resolve), schema_for!(Committed)),
            ToolName::RequestReview => (schema_for!(RequestReview), schema_for!(Committed)),
            ToolName::SubscribeEvents => (schema_for!(SubscribeEvents), schema_for!(Events)),
        }
    }

    #[must_use]
    pub fn tool(self) -> Tool {
        let (input_schema, output_schema) = self.schemas();
        let description = input_schema
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
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

// ---- arguments -------------------------------------------------------------

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

/// Create a review over one or more repos. Returns the new review. Without
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

/// Full contents of a file at the review's base or head, numbered. Works
/// for unchanged files too.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetFile {
    pub review_id: ReviewId,
    /// Needed only when the review spans several repos.
    pub repo_id: Option<RepoId>,
    /// Path relative to the repo root.
    pub path: String,
    /// Default `Head`.
    #[serde(default = "head")]
    pub side: Side,
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
/// that name are notified.
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
/// `review_id`, `workspace_id`, and `awaiting_agent` are mutually exclusive:
/// provide at most one non-null scope filter, or omit all for every event.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(try_from = "SubscribeEventsWire")]
pub struct SubscribeEvents {
    pub scope: SubscribeScope,
    pub since_seq: Option<Seq>,
    pub timeout_ms: u64,
    pub max: usize,
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
    /// exclusive with `review_id` and `workspace_id`.
    awaiting_agent: Option<String>,
    /// Replay after this log position; omit for live only.
    since_seq: Option<Seq>,
    /// Default 30000.
    #[serde(default = "default_timeout")]
    timeout_ms: u64,
    /// Default 100.
    #[serde(default = "default_max")]
    max: usize,
}

#[derive(Debug, thiserror::Error)]
#[error(
    "review_id, workspace_id, and awaiting_agent are mutually exclusive; provide at most one non-null scope filter, or omit all for every event"
)]
struct ConflictingSubscribeScopes;

impl TryFrom<SubscribeEventsWire> for SubscribeEvents {
    type Error = ConflictingSubscribeScopes;

    fn try_from(wire: SubscribeEventsWire) -> Result<Self, Self::Error> {
        let scope = match (wire.review_id, wire.workspace_id, wire.awaiting_agent) {
            (None, None, None) => SubscribeScope::All,
            (Some(review_id), None, None) => SubscribeScope::Review { review_id },
            (None, Some(workspace_id), None) => SubscribeScope::Workspace { workspace_id },
            (None, None, Some(agent)) => SubscribeScope::AwaitingAgent { agent },
            (Some(_), Some(_), _) | (Some(_), None, Some(_)) | (None, Some(_), Some(_)) => {
                return Err(ConflictingSubscribeScopes);
            }
        };
        Ok(Self {
            scope,
            since_seq: wire.since_seq,
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

#[derive(Debug, Serialize, JsonSchema)]
pub struct Workspaces {
    pub workspaces: Vec<Workspace>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Reviews {
    pub reviews: Vec<Review>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ReviewDetail {
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
    pub review: Review,
    pub resolved: Option<NonEmpty<ResolvedTarget>>,
    pub event: Event,
}

/// The committed event.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Committed {
    pub event: Event,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DiffText {
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
    pub repo_id: RepoId,
    pub path: RepoPath,
    pub side: Side,
    pub blob_oid: BlobOid,
    pub lang: Option<String>,
    pub content: RenderContent,
    /// Numbered file text.
    pub text: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Comments {
    pub threads: Vec<Thread>,
    pub comments: Vec<Comment>,
    pub seq: Seq,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct NewThread {
    pub comment_id: CommentId,
    pub thread_id: ThreadId,
    pub event: Event,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Replied {
    pub comment_id: CommentId,
    pub event: Event,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct Events {
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
            assert_eq!(parsed.since_seq, None);
            assert_eq!(parsed.timeout_ms, 30_000);
            assert_eq!(parsed.max, 100);
        }

        let parsed: SubscribeEvents = serde_json::from_value(serde_json::json!({
            "review_id": review_id, "since_seq": 7, "timeout_ms": 5, "max": 3
        }))
        .unwrap();
        assert_eq!(parsed.scope, SubscribeScope::Review { review_id });
        assert_eq!(parsed.since_seq, Some(Seq::new(7)));
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
