//! Example values for every `nits-client-core` type that crosses the host ↔
//! UI boundary (`ViewModel`, `Action` and everything they contain), written
//! to `fixtures/client/` by `cargo xtask fixtures` and consumed by the
//! `ReScript` boundary test. Same registry shape as
//! `nits-protocol-fixtures`; protocol values inside view types come from
//! that crate's examples so the two fixture sets never disagree.

#![allow(clippy::wildcard_imports)] // this crate is a registry over every type

use nits_client_core::*;
use nits_protocol::*;
use nits_protocol_fixtures::{Fixture, FixtureError, Fixtures as ProtoFixtures, Registered};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use strum::IntoEnumIterator;

/// Same contract as `nits_protocol_fixtures::Fixtures`, defined here so it
/// can be implemented for `nits-client-core` types (orphan rule).
pub trait Fixtures: Sized + Serialize + DeserializeOwned {
    const TYPE_NAME: &'static str;
    fn examples() -> Result<Vec<(String, Self)>, FixtureError>;
    fn required_names() -> Vec<String> {
        vec!["default".to_owned()]
    }
    fn name_of(&self) -> String {
        "default".to_owned()
    }
    fn fixtures() -> Result<Vec<Fixture>, FixtureError> {
        Self::examples()?
            .into_iter()
            .map(|(name, v)| {
                Ok(Fixture {
                    type_name: Self::TYPE_NAME,
                    name,
                    value: serde_json::to_value(v)?,
                })
            })
            .collect()
    }
}

/// Round-trip a fixture through the type it belongs to.
pub fn roundtrip<T: Fixtures>(value: &Value) -> Result<Value, serde_json::Error> {
    let t: T = serde_json::from_value(value.clone())?;
    serde_json::to_value(t)
}

/// `struct_fixture!(Type, "Type", expr)`: one example named `default`.
macro_rules! struct_fixture {
    ($ty:ty, $name:literal, $build:expr) => {
        impl Fixtures for $ty {
            const TYPE_NAME: &'static str = $name;
            fn examples() -> Result<Vec<(String, Self)>, FixtureError> {
                Ok(vec![("default".to_owned(), $build)])
            }
        }
    };
}

/// `unit_enum_fixture!(Type, "Type")`: every variant of an `EnumIter` enum.
macro_rules! unit_enum_fixture {
    ($ty:ty, $name:literal) => {
        impl Fixtures for $ty {
            const TYPE_NAME: &'static str = $name;
            fn examples() -> Result<Vec<(String, Self)>, FixtureError> {
                Ok(<$ty>::iter().map(|v| (format!("{v:?}"), v)).collect())
            }
            fn required_names() -> Vec<String> {
                <$ty>::iter().map(|v| format!("{v:?}")).collect()
            }
            fn name_of(&self) -> String {
                format!("{self:?}")
            }
        }
    };
}

/// `enum_fixture!(Type, TypeKind, "Type", [example, ...])`: payload enum;
/// coverage is checked against the strum discriminant list.
macro_rules! enum_fixture {
    ($ty:ty, $kind:ty, $name:literal, [$($v:expr),* $(,)?]) => {
        impl Fixtures for $ty {
            const TYPE_NAME: &'static str = $name;
            fn examples() -> Result<Vec<(String, Self)>, FixtureError> {
                let vs: Vec<Self> = vec![$($v),*];
                Ok(vs.into_iter().map(|v| (v.name_of(), v)).collect())
            }
            fn required_names() -> Vec<String> {
                <$kind>::iter().map(|k| format!("{k:?}")).collect()
            }
            fn name_of(&self) -> String {
                format!("{:?}", <$kind>::from(self))
            }
        }
    };
}

fn missing_names<T: Fixtures>() -> Result<Vec<String>, FixtureError> {
    let have: std::collections::BTreeSet<String> =
        T::examples()?.iter().map(|(_, v)| v.name_of()).collect();
    Ok(T::required_names()
        .into_iter()
        .filter(|n| !have.contains(n))
        .collect())
}

/// `registry!(TypeA, ...)` builds `registry()`; see `nits-protocol-fixtures`.
macro_rules! registry {
    ($($ty:ty),* $(,)?) => {
        /// All fixture-bearing client types.
        #[must_use]
        pub fn registry() -> Vec<Registered> {
            vec![$(Registered {
                type_name: <$ty as Fixtures>::TYPE_NAME,
                fixtures: <$ty as Fixtures>::fixtures,
                roundtrip: roundtrip::<$ty>,
                missing_names: missing_names::<$ty>,
            }),*]
        }
    };
}

registry!(
    ViewModel,
    VisualView,
    ScrollIntent,
    ScrollAlign,
    Landing,
    ContentSearchView,
    RefSelectorSide,
    RefSelectorStatus,
    RefOption,
    RefSelectorView,
    ViewPrefs,
    Layout,
    Tab,
    Mode,
    ConnectionView,
    Draft,
    DraftPurpose,
    PendingEvent,
    OpenReview,
    OpenFile,
    RenderKey,
    FileRef,
    TreeView,
    TreeNode,
    SearchView,
    SearchHit,
    Progress,
    ViewedState,
    ChangeKindKind,
    DiffView,
    DiffRow,
    RowThread,
    RowPlace,
    CommentView,
    ThreadView,
    ThreadPlace,
    CommitStepper,
    StepperCommit,
    Focus,
    Hint,
    HelpView,
    HelpGroup,
    HelpEntry,
    Conflict,
    Context,
    Command,
    Action,
    ScopeChoice,
    Override,
    Overrides,
    ViewDelta,
    ViewPatch,
    KeyChord,
    KeyCode,
    NamedKey,
    Modifiers,
);

/// All fixtures of all registered types.
pub fn all() -> Result<Vec<Fixture>, FixtureError> {
    let mut out = Vec::new();
    for r in registry() {
        out.extend((r.fixtures)()?);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Example builders. Protocol values come from `nits-protocol-fixtures`.
// ---------------------------------------------------------------------------

/// The first protocol example of `T`.
fn proto<T: ProtoFixtures>() -> Result<T, FixtureError> {
    T::examples()?
        .into_iter()
        .next()
        .map(|(_, v)| v)
        .ok_or(FixtureError::ZeroLine)
}

/// The protocol example of `T` named `name`.
fn proto_named<T: ProtoFixtures>(name: &str) -> Result<T, FixtureError> {
    T::examples()?
        .into_iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v)
        .ok_or(FixtureError::ZeroLine)
}

/// The first client example of `T`.
fn local<T: Fixtures>() -> Result<T, FixtureError> {
    T::examples()?
        .into_iter()
        .next()
        .map(|(_, v)| v)
        .ok_or(FixtureError::ZeroLine)
}

/// The client example of `T` named `name`.
fn local_named<T: Fixtures>(name: &str) -> Result<T, FixtureError> {
    T::examples()?
        .into_iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v)
        .ok_or(FixtureError::ZeroLine)
}

fn repo_id() -> Result<RepoId, FixtureError> {
    Ok(proto::<Repo>()?.id)
}
fn review_id() -> Result<ReviewId, FixtureError> {
    Ok(proto::<Review>()?.id)
}
fn thread_id() -> Result<ThreadId, FixtureError> {
    Ok(proto::<Thread>()?.id)
}
fn comment_id() -> Result<CommentId, FixtureError> {
    Ok(proto::<Comment>()?.id)
}
fn commit_oid() -> Result<CommitOid, FixtureError> {
    Ok(proto::<CommitInfo>()?.oid)
}
fn path(s: &str) -> Result<RepoPath, FixtureError> {
    Ok(RepoPath::new(s)?)
}
fn file_ref() -> Result<FileRef, FixtureError> {
    Ok(FileRef {
        repo_id: repo_id()?,
        path: path("src/lib.rs")?,
    })
}
fn render_key() -> Result<RenderKey, FixtureError> {
    Ok(RenderKey {
        repo_id: repo_id()?,
        path: path("src/lib.rs")?,
        target: proto_named::<RenderTarget>("Diff")?,
        opts: RenderOpts::default(),
    })
}
fn keys(s: &str) -> Result<KeySeq, FixtureError> {
    s.parse().map_err(|_| FixtureError::ZeroLine)
}

struct_fixture!(
    ViewPrefs,
    "ViewPrefs",
    ViewPrefs {
        layout: Layout::Split,
        ignore_whitespace: true,
        context_lines: 5,
        sidebar_hidden: false,
    }
);
unit_enum_fixture!(Layout, "Layout");
unit_enum_fixture!(Tab, "Tab");
unit_enum_fixture!(Mode, "Mode");
enum_fixture!(
    ConnectionView,
    ConnectionViewKind,
    "ConnectionView",
    [
        ConnectionView::Disconnected,
        ConnectionView::Connecting,
        ConnectionView::Subscribed,
        ConnectionView::Rejected {
            error: proto_named::<RpcError>("UnsupportedProtocol")?,
        },
    ]
);
enum_fixture!(
    DraftPurpose,
    DraftPurposeKind,
    "DraftPurpose",
    [
        DraftPurpose::Comment {
            intent: nits_protocol::CommentIntent::Finding,
            context: None
        },
        DraftPurpose::Reply {
            thread_id: thread_id()?
        },
        DraftPurpose::Defer {
            thread_id: thread_id()?
        },
    ]
);
struct_fixture!(
    Draft,
    "Draft",
    Draft {
        purpose: DraftPurpose::Reply {
            thread_id: thread_id()?
        },
        anchor: proto_named::<Anchor>("Lines")?,
    }
);
struct_fixture!(
    PendingEvent,
    "PendingEvent",
    PendingEvent {
        client_seq: ClientSeq::new(7),
        body: proto_named::<EventBody>("CommentCreated")?,
    }
);
struct_fixture!(
    OpenFile,
    "OpenFile",
    OpenFile {
        render: render_key()?,
        first_row: 120,
        last_row: 179,
    }
);
struct_fixture!(RenderKey, "RenderKey", render_key()?);
struct_fixture!(
    ContentSearchView,
    "ContentSearchView",
    ContentSearchView {
        query: "todo".into(),
        all_files: true,
        hits: vec![proto::<ContentHit>()?],
        truncated: false,
        pending: false,
        selected: 0,
    }
);
unit_enum_fixture!(RefSelectorSide, "RefSelectorSide");
enum_fixture!(
    RefSelectorStatus,
    RefSelectorStatusKind,
    "RefSelectorStatus",
    [
        RefSelectorStatus::Loading,
        RefSelectorStatus::Ready,
        RefSelectorStatus::Saving,
        RefSelectorStatus::InvalidRef {
            message: "unknown revision".into()
        },
        RefSelectorStatus::DaemonError {
            message: "git unavailable".into()
        },
    ]
);
struct_fixture!(
    RefOption,
    "RefOption",
    RefOption {
        ref_spec: RefSpec::Branch {
            name: "main".into()
        },
        subject: None,
        current: true,
    }
);
struct_fixture!(
    RefSelectorView,
    "RefSelectorView",
    RefSelectorView {
        repo_id: repo_id()?,
        repo_name: "nits".into(),
        side: RefSelectorSide::Head,
        current: RefSpec::Branch {
            name: "main".into()
        },
        query: "sel".into(),
        options: vec![local::<RefOption>()?],
        selected: 0,
        status: RefSelectorStatus::Ready,
    }
);
struct_fixture!(FileRef, "FileRef", file_ref()?);
struct_fixture!(
    OpenReview,
    "OpenReview",
    OpenReview {
        snapshot: proto::<ReviewSnapshot>()?,
        pending: vec![local::<PendingEvent>()?],
        trees: vec![proto::<TreeSnapshot>()?.root_oid],
        files: vec![render_key()?],
        open_file: Some(local::<OpenFile>()?),
        scope: DiffScope::All,
        scoped_targets: Vec::new(),
        original: None,
    }
);
unit_enum_fixture!(ViewedState, "ViewedState");
unit_enum_fixture!(ChangeKindKind, "ChangeKindKind");
enum_fixture!(
    TreeNode,
    TreeNodeKind,
    "TreeNode",
    [
        TreeNode::Dir {
            name: "src".into(),
            repo_id: repo_id()?,
            path: Some(path("src")?),
            expanded: true,
            changed_below: 1,
            children: vec![TreeNode::File {
                name: "lib.rs".into(),
                repo_id: repo_id()?,
                path: path("src/lib.rs")?,
                change: Some(ChangeKindKind::Modified),
                viewed: ViewedState::ChangedSinceViewed,
                open: true,
                additions: Some(9),
                deletions: Some(1),
                threads: 2,
            }],
        },
        TreeNode::File {
            name: "README.md".into(),
            repo_id: repo_id()?,
            path: path("README.md")?,
            change: None,
            viewed: ViewedState::Unviewed,
            open: false,
            additions: None,
            deletions: None,
            threads: 0,
        },
    ]
);
struct_fixture!(
    SearchHit,
    "SearchHit",
    SearchHit {
        file: file_ref()?,
        matched: vec![0, 4, 5],
        change: Some(ChangeKindKind::Modified),
    }
);
struct_fixture!(
    SearchView,
    "SearchView",
    SearchView {
        query: "sli".into(),
        hits: vec![local::<SearchHit>()?],
        selected: 0,
    }
);
struct_fixture!(
    TreeView,
    "TreeView",
    TreeView {
        roots: vec![TreeNode::Dir {
            name: proto::<Repo>()?.id.to_string(),
            repo_id: repo_id()?,
            path: None,
            expanded: true,
            changed_below: 1,
            children: TreeNode::examples()?.into_iter().map(|(_, v)| v).collect(),
        }],
        breadcrumbs: vec![
            proto::<Repo>()?.id.to_string(),
            "src".into(),
            "lib.rs".into()
        ],
        search: Some(local::<SearchView>()?),
    }
);
struct_fixture!(
    Progress,
    "Progress",
    Progress {
        viewed: 3,
        changed_since_viewed: 1,
        total: 12,
        additions: 64,
        deletions: 12,
    }
);
struct_fixture!(
    DiffRow,
    "DiffRow",
    DiffRow {
        index: 121,
        row: proto_named::<Row>("Modified")?,
        threads: vec![local::<RowThread>()?],
        drafted: Some((RowPlace::Anchor, Side::Head)),
    }
);
unit_enum_fixture!(RowPlace, "RowPlace");
struct_fixture!(
    RowThread,
    "RowThread",
    RowThread {
        thread: thread_id()?,
        side: Side::Base,
        place: RowPlace::Anchor,
    }
);
struct_fixture!(
    DiffView,
    "DiffView",
    DiffView {
        target: proto_named::<RenderTarget>("Diff")?,
        file: file_ref()?,
        lang: Some("Rust".into()),
        content: proto_named::<RenderContent>("Text")?,
        viewed: ViewedState::ChangedSinceViewed,
        first_row: 120,
        last_row: 179,
        rows: vec![local::<DiffRow>()?],
        missing: vec![ChunkIndex::new(1)],
        file_threads: vec![thread_id()?],
        collapsed: false,
        original: false,
    }
);
enum_fixture!(
    ThreadPlace,
    ThreadPlaceKind,
    "ThreadPlace",
    [
        ThreadPlace::Review,
        ThreadPlace::File { file: file_ref()? },
        ThreadPlace::Lines {
            file: file_ref()?,
            side: Side::Head,
            start: 10,
            end: 12,
        },
    ]
);
struct_fixture!(
    ThreadView,
    "ThreadView",
    ThreadView {
        id: thread_id()?,
        root: comment_id()?,
        author: proto_named::<Author>("Human")?,
        created: proto::<Comment>()?.created,
        summary: "This should be a newtype.".into(),
        replies: 1,
        status: ThreadStatus::Open,
        place: local_named::<ThreadPlace>("Lines")?,
        outdated: false,
        pending: true,
        suggestion: true,
        comments: vec![local::<CommentView>()?],
        context: proto::<Comment>()?.context,
    }
);
struct_fixture!(CommentView, "CommentView", {
    let c = proto::<Comment>()?;
    CommentView {
        id: c.id,
        author: c.author,
        created: c.created,
        body: c.body,
        pending: false,
    }
});
struct_fixture!(StepperCommit, "StepperCommit", {
    let c = proto::<CommitInfo>()?;
    StepperCommit {
        oid: c.oid,
        parents: c.parents,
        subject: c.subject,
        body: c.body,
        author: c.author.name,
        time: c.author.time,
        committer: c.committer.name,
        committer_time: c.committer.time,
    }
});
struct_fixture!(
    CommitStepper,
    "CommitStepper",
    CommitStepper {
        repo_id: repo_id()?,
        commits: vec![local::<StepperCommit>()?],
        has_worktree: true,
    }
);
enum_fixture!(
    Focus,
    FocusKind,
    "Focus",
    [
        Focus::ReviewList { index: 0 },
        Focus::Tree { index: 2 },
        Focus::Diff {
            row: 121,
            side: Side::Head,
        },
        Focus::Thread { index: 0 },
        Focus::ReviewRequest { index: 0 },
        Focus::Composer,
        Focus::CommitStepper { index: 0 },
        Focus::Help,
    ]
);
struct_fixture!(
    Hint,
    "Hint",
    Hint {
        keys: "] f".into(),
        command: Command::NextFile,
        label: "next file".into(),
    }
);
struct_fixture!(
    HelpEntry,
    "HelpEntry",
    HelpEntry {
        keys: "g g".into(),
        command: Command::GoTop,
        label: "top".into(),
        primary: false,
        overridden: true,
    }
);
struct_fixture!(
    HelpGroup,
    "HelpGroup",
    HelpGroup {
        context: Context::Diff,
        entries: vec![local::<HelpEntry>()?],
    }
);
struct_fixture!(
    Conflict,
    "Conflict",
    Conflict {
        context: Context::Diff,
        keys: keys("j")?,
        commands: vec![Command::MoveDown, Command::PrevHunk],
    }
);
struct_fixture!(
    HelpView,
    "HelpView",
    HelpView {
        groups: vec![local::<HelpGroup>()?],
        conflicts: vec![local::<Conflict>()?],
    }
);
unit_enum_fixture!(Context, "Context");
unit_enum_fixture!(Command, "Command");

struct_fixture!(
    Override,
    "Override",
    Override {
        context: Context::Diff,
        command: Command::Comment,
        keys: Some(keys("x")?),
        primary: true,
    }
);
struct_fixture!(
    Overrides,
    "Overrides",
    Overrides {
        bindings: vec![local::<Override>()?],
    }
);
struct_fixture!(
    ViewDelta,
    "ViewDelta",
    ViewDelta::new(&[ViewSection::Diff, ViewSection::Threads,])
);
enum_fixture!(
    Action,
    ActionKind,
    "Action",
    [
        Action::Connect,
        Action::Disconnect,
        Action::ListWorkspaces,
        Action::ListReviews {
            workspace_id: proto::<Workspace>()?.id,
        },
        Action::CreateReview {
            workspace_id: proto::<Workspace>()?.id,
            title: "Keyboard-first client core".into(),
            targets: proto::<Review>()?.targets,
        },
        Action::OpenReview {
            review_id: review_id()?,
        },
        Action::CloseReview,
        Action::InformationalNoteOpened,
        Action::DraftOpened {
            anchor: proto_named::<Anchor>("Lines")?,
        },
        Action::DraftSubmitted {
            body: "Looks good.".into(),
        },
        Action::DraftDiscarded,
        Action::ReplyOpened {
            thread_id: thread_id()?,
        },
        Action::SetFocus {
            focus: Focus::Diff {
                row: 121,
                side: Side::Head,
            },
        },
        Action::ToggleHelp,
        Action::Reply {
            thread_id: thread_id()?,
            body: "Agreed.".into(),
        },
        Action::EditComment {
            comment_id: comment_id()?,
            body: "Edited body.".into(),
        },
        Action::DeleteComment {
            comment_id: comment_id()?,
        },
        Action::DeferOpened {
            thread_id: thread_id()?
        },
        Action::DeferThread {
            thread_id: thread_id()?,
            reason: "Deferred to controller issue per scope decision"
                .parse()
                .map_err(FixtureError::Invalid)?,
            tracking_url: Some(
                "https://example.com/issues/288"
                    .parse()
                    .map_err(FixtureError::Invalid)?
            )
        },
        Action::ResolveThread {
            thread_id: thread_id()?,
        },
        Action::UnresolveThread {
            thread_id: thread_id()?,
        },
        Action::ApplySuggestion {
            comment_id: comment_id()?,
        },
        Action::Viewport {
            file: file_ref()?,
            first_row: 120,
            last_row: 179,
        },
        Action::OpenFileAt {
            file: file_ref()?,
            row: 121,
            side: Side::Base,
            landing: Landing::Pin,
        },
        Action::CloseFile,
        Action::ToggleDir {
            repo_id: repo_id()?,
            path: Some(path("src")?),
        },
        Action::FileSearch {
            query: Some("sli".into()),
        },
        Action::SetLayout {
            layout: Layout::Split,
        },
        Action::SetRenderOpts {
            ignore_whitespace: true,
            context_lines: 5,
        },
        Action::CommentLines {
            file: file_ref()?,
            side: Side::Head,
            start_line: 10,
            end_line: 12,
        },
        Action::CommentFile { file: file_ref()? },
        Action::SetTab {
            tab: Tab::Conversation,
        },
        Action::ToggleSidebar,
        Action::CopyPath {
            path: path("src/lib.rs")?,
        },
        Action::ScrollView {
            align: ScrollAlign::Center,
        },
        Action::ToggleFileCollapse { file: file_ref()? },
        Action::CollapseParent,
        Action::CollapseAll,
        Action::MarkViewed { file: file_ref()? },
        Action::UnmarkViewed { file: file_ref()? },
        Action::ListCommits {
            repo_id: repo_id()?,
        },
        Action::StepCommit { selected: Some(0) },
        Action::SetScope {
            scope: ScopeChoice::ByCommit
        },
        Action::OpenOriginalDiff {
            thread_id: thread_id()?
        },
        Action::ExpandContext {
            file: file_ref()?,
            full: false
        },
        Action::ExpandGap {
            file: file_ref()?,
            gap: Gap::new(1),
            dir: ExpandDir::Up
        },
        Action::SetBrowseRef {
            repo_id: repo_id()?,
            ref_spec: Some(RefSpec::Branch {
                name: "main".into()
            })
        },
        Action::ContentSearch {
            query: Some("todo".into()),
            all_files: false
        },
        Action::ActionPalette { open: true },
        Action::RunCommand {
            command: Command::ToggleLayout
        },
        Action::EnterVisual,
        Action::LeaveVisual,
        Action::SearchFirst {
            search: nits_client_core::SearchKind::Files
        },
        Action::SearchStep {
            search: nits_client_core::SearchKind::Files,
            delta: 1
        },
        Action::OpenSearchResult {
            search: nits_client_core::SearchKind::Content,
            query: "todo".into()
        },
        Action::OpenRefSelector {
            repo_id: repo_id()?,
            side: RefSelectorSide::Base,
        },
        Action::RefSelectorQuery {
            query: "main".into()
        },
        Action::RefSelectorStep { delta: 1 },
        Action::SelectRef { index: 0 },
        Action::SelectCurrentRef,
        Action::CloseRefSelector,
    ]
);
enum_fixture!(
    ScopeChoice,
    ScopeChoiceKind,
    "ScopeChoice",
    [
        ScopeChoice::All,
        ScopeChoice::Committed,
        ScopeChoice::ByCommit,
        ScopeChoice::Commit {
            repo_id: repo_id()?,
            oid: commit_oid()?
        },
        ScopeChoice::Worktree {
            repo_id: repo_id()?
        },
    ]
);
unit_enum_fixture!(NamedKey, "NamedKey");
struct_fixture!(
    Modifiers,
    "Modifiers",
    Modifiers {
        ctrl: true,
        alt: false,
        shift: true,
        meta: false,
    }
);
enum_fixture!(
    KeyCode,
    KeyCodeKind,
    "KeyCode",
    [
        KeyCode::Char { c: 'g' },
        KeyCode::Named {
            key: NamedKey::Enter,
        },
    ]
);
struct_fixture!(
    KeyChord,
    "KeyChord",
    KeyChord {
        key: KeyCode::Char { c: 'p' },
        mods: Modifiers {
            ctrl: true,
            alt: false,
            shift: false,
            meta: false,
        },
    }
);
enum_fixture!(
    ViewPatch,
    ViewPatchKind,
    "ViewPatch",
    [
        ViewPatch::Connection {
            connection: ConnectionView::Subscribed,
            last_error: Some(proto_named::<RpcError>("NotFound")?),
        },
        ViewPatch::ReviewList {
            workspaces: vec![proto::<Workspace>()?],
            reviews: vec![proto::<Review>()?],
            open_review: Some(proto::<Review>()?.id),
            resolved_targets: vec![proto::<ResolvedTarget>()?],
            scope: DiffScope::All,
            browse_ref: Some(RefSpec::Branch {
                name: "main".into()
            }),
        },
        ViewPatch::Search {
            content_search: Some(local::<ContentSearchView>()?),
            action_palette: true,
        },
        ViewPatch::Tree {
            tree: local::<TreeView>()?,
        },
        ViewPatch::Diff {
            diff: Some(local::<DiffView>()?),
            diffs: vec![local::<DiffView>()?],
            prefs: local::<ViewPrefs>()?,
            visual: Some(local::<VisualView>()?),
        },
        ViewPatch::Threads {
            threads: vec![local::<ThreadView>()?],
        },
        ViewPatch::Conversation {
            requests: vec![proto::<ReviewRequest>()?],
            conversation: vec![local::<ThreadView>()?],
        },
        ViewPatch::CommitStepper {
            stepper: Some(local::<CommitStepper>()?),
        },
        ViewPatch::RefSelector {
            ref_selector: Some(local::<RefSelectorView>()?),
        },
        ViewPatch::Progress {
            progress: local::<Progress>()?,
        },
        ViewPatch::Focus {
            focus: Focus::Diff {
                row: 121,
                side: Side::Head,
            },
            tab: Tab::FilesChanged,
            scroll: Some(local::<ScrollIntent>()?),
            copy_target: Some(path("src/lib.rs")?),
        },
        ViewPatch::Hints {
            hints: vec![local::<Hint>()?],
            pending: "g".into(),
            pending_label: Some("Go".into()),
            mode: Mode::Normal,
            leader: "space".into(),
            chrome: vec![local::<Hint>()?],
            bindings: vec![local::<Hint>()?],
            last_key: Some(LastKey {
                seq: 12,
                command: Some(Command::CopyPath),
            }),
        },
        ViewPatch::Help {
            help: Some(local::<HelpView>()?),
        },
        ViewPatch::Draft {
            draft: Some(local::<Draft>()?),
            pending_refresh: true,
        },
    ]
);
struct_fixture!(
    VisualView,
    "VisualView",
    VisualView {
        start: 4,
        end: 6,
        side: Side::Head,
    }
);
struct_fixture!(
    ScrollIntent,
    "ScrollIntent",
    ScrollIntent {
        row: 121,
        align: ScrollAlign::Center,
        seq: 3,
    }
);
unit_enum_fixture!(ScrollAlign, "ScrollAlign");
unit_enum_fixture!(Landing, "Landing");
struct_fixture!(
    ViewModel,
    "ViewModel",
    ViewModel {
        prefs: local::<ViewPrefs>()?,
        tree: local::<TreeView>()?,
        progress: local::<Progress>()?,
        diff: Some(local::<DiffView>()?),
        diffs: vec![local::<DiffView>()?],
        threads: vec![local::<ThreadView>()?],
        conversation: Vec::new(),
        requests: vec![proto::<ReviewRequest>()?],
        stepper: Some(local::<CommitStepper>()?),
        focus: Focus::Diff {
            row: 121,
            side: Side::Head,
        },
        tab: Tab::FilesChanged,
        scroll: Some(local::<ScrollIntent>()?),
        mode: Mode::Normal,
        hints: vec![local::<Hint>()?],
        pending_keys: String::new(),
        pending_label: None,
        leader: "space".into(),
        chrome: vec![local::<Hint>()?],
        bindings: vec![local::<Hint>()?],
        last_key: Some(LastKey {
            seq: 12,
            command: Some(Command::CopyPath),
        }),
        help: None,
        copy_target: Some(path("src/lib.rs")?),
        connection: ConnectionView::Subscribed,
        last_error: None,
        workspaces: vec![proto::<Workspace>()?],
        reviews: vec![proto::<Review>()?],
        open_review: Some(proto::<Review>()?.id),
        resolved_targets: vec![proto::<ResolvedTarget>()?],
        scope: DiffScope::All,
        browse_ref: None,
        content_search: Some(local::<ContentSearchView>()?),
        action_palette: false,
        ref_selector: Some(local::<RefSelectorView>()?),
        visual: Some(local::<VisualView>()?),
        review: Some(local::<OpenReview>()?),
        draft: Some(local::<Draft>()?),
        pending_refresh: true,
    }
);
