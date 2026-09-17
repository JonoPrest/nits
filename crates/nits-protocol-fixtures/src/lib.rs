//! Example values for every `nits-protocol` type: one per enum variant,
//! written to `fixtures/protocol/` by `cargo xtask fixtures` and consumed by
//! the `ReScript` boundary test.
//!
//! This is a separate crate so `nits-protocol` ships only wire types; sample
//! data never reaches a daemon, client, or wasm build.
//!
//! Every enum gets a fixture per variant; coverage is enforced by comparing
//! against the strum-generated discriminant list, so adding a variant without
//! a fixture fails `fixtures_cover_every_variant`.

#![allow(clippy::wildcard_imports)] // this crate is a registry over every type

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use strum::IntoEnumIterator;

use nits_protocol::domain::*;
use nits_protocol::events::*;
use nits_protocol::ids::*;
use nits_protocol::invariants::*;
use nits_protocol::render::*;
use nits_protocol::rpc::*;
use nits_protocol::version::*;

/// Why a fixture could not be built or serialised.
#[derive(Debug, thiserror::Error)]
pub enum FixtureError {
    #[error("invalid fixture text: {0}")]
    Invalid(String),

    #[error("invalid reference fixture: {0}")]
    Reference(#[from] nits_protocol::ReferenceError),
    #[error("invalid fixture value: {0}")]
    Invariant(#[from] InvariantError),
    #[error("line number 0 in fixture")]
    ZeroLine,
    #[error("fixture does not serialise: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid gap table in fixture: {0}")]
    GapTable(#[from] nits_protocol::GapTableError),
}

/// A named example value of a protocol type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fixture {
    /// Type name; directory under `fixtures/protocol/`.
    pub type_name: &'static str,
    /// Variant name (or `default` for structs); file stem.
    pub name: String,
    pub value: Value,
}

impl Fixture {
    /// Relative path of this fixture under `fixtures/protocol/`.
    #[must_use]
    pub fn rel_path(&self) -> String {
        format!("{}/{}.json", self.type_name, self.name)
    }
}

/// A type that can describe its own example values.
pub trait Fixtures: Sized + Serialize + DeserializeOwned {
    const TYPE_NAME: &'static str;

    /// Example values, each with a name. For enums, names are variant names.
    ///
    /// Fallible so that helpers building paths/ranges can propagate an
    /// `InvariantError` instead of panicking; a bad fixture is a test
    /// failure with a message, not a crash in `cargo xtask`.
    fn examples() -> Result<Vec<(String, Self)>, FixtureError>;

    /// Names every example must collectively cover. Enums return their
    /// variant names; structs return `["default"]`.
    fn required_names() -> Vec<String> {
        vec!["default".to_owned()]
    }

    /// The name this particular value should carry. Enums return the
    /// variant name; structs return `"default"`.
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
///
/// Returns the re-serialised value so callers can assert it is identical.
pub fn roundtrip<T: Fixtures>(value: &Value) -> Result<Value, serde_json::Error> {
    let t: T = serde_json::from_value(value.clone())?;
    serde_json::to_value(t)
}

/// Every registered type, as `(type_name, fixtures, roundtrip, coverage)`.
///
/// `coverage` returns the names required but missing from the examples.
#[derive(Clone, Copy)]
pub struct Registered {
    pub type_name: &'static str,
    pub fixtures: fn() -> Result<Vec<Fixture>, FixtureError>,
    pub roundtrip: fn(&Value) -> Result<Value, serde_json::Error>,
    pub missing_names: fn() -> Result<Vec<String>, FixtureError>,
}

impl core::fmt::Debug for Registered {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Registered")
            .field("type_name", &self.type_name)
            .finish_non_exhaustive()
    }
}

fn missing_names<T: Fixtures>() -> Result<Vec<String>, FixtureError> {
    let have: std::collections::BTreeSet<String> =
        T::examples()?.iter().map(|(_, v)| v.name_of()).collect();
    Ok(T::required_names()
        .into_iter()
        .filter(|n| !have.contains(n))
        .collect())
}

/// `registry!(TypeA, TypeB, ...)` builds `registry()`: a `Vec<Registered>`
/// with one entry per listed type, each holding function pointers to that
/// type's `fixtures`, JSON `roundtrip`, and `missing_names` (coverage).
/// Tests and `cargo xtask fixtures` iterate this list, so a type that
/// implements `Fixtures` but is not listed here produces no files.
macro_rules! registry {
    ($($ty:ty),* $(,)?) => {
        /// All fixture-bearing protocol types.
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
    // domain
    Workspace,
    Repo,
    RefSpec,
    BaseRefSpec,
    TargetRevision,
    ReviewTargetUpdate,
    RefCandidate,
    ResolvedRef,
    ResolvedSource,
    ReviewTarget,
    ResolvedTarget,
    ReviewStatus,
    Review,
    Sig,
    CommitInfo,
    Human,
    AgentVia,
    Author,
    Side,
    Anchor,
    CommentKind,
    CommentIntent,
    CommentState,
    CommentContext,
    Comment,
    ThreadResolution,
    Thread,
    ViewedMark,
    RenderOpts,
    GapExpansion,
    GapRow,
    ChangeKind,
    DiffScope,
    ContentHit,
    FileChange,
    TreeEntryKind,
    TreeEntry,
    TreeSnapshot,
    TreeDelta,
    // events
    Event,
    EventBody,
    // render
    SpanClass,
    Span,
    Cell,
    ExpandDir,
    Row,
    RenderTarget,
    RenderContent,
    FileRenderHeader,
    RenderChunk,
    FileRender,
    FileSummary,
    DiffSummary,
    // rpc
    ClientMsg,
    ServerMsg,
    Since,
    SubscribeScope,
    Mutation,
    Request,
    EnsureDirectoryReview,
    DirectoryReview,
    DirectoryReviewOutcome,
    ReviewSnapshot,
    ReviewRequest,
    ReviewRequestId,
    Response,
    StreamItem,
    EntityKind,
    RpcError,
    ViewSection,
    // version
    ProtocolVersion,
    SchemaVersion,
    BuildInfo,
    UpgradeNotice,
    Envelope<ClientMsg>,
    Envelope<ServerMsg>,
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
// Helpers for building example values. Deterministic: fixed ids and times.
// ---------------------------------------------------------------------------

/// `struct_fixture!(Type, "Type", expr)` registers a struct with one example
/// named `default`. `expr` is evaluated inside a function returning
/// `Result<_, FixtureError>`, so helpers may use `?`.
///
/// Expands to `impl Fixtures for Type` with `examples()` returning
/// `[("default", expr)]`; `required_names`/`name_of` keep their struct
/// defaults (`"default"`).
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

/// `unit_enum_fixture!(Type, "Type")` registers an enum whose variants are
/// all unit variants and which derives strum's `EnumIter`.
///
/// No example list is needed: `examples()` iterates every variant, so
/// coverage is automatic and the fixture name is the variant's `Debug` name.
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

/// `enum_fixture!(Type, TypeKind, "Type", [example, ...])` registers an enum
/// with payload-carrying variants.
///
/// `TypeKind` is the strum `EnumDiscriminants` type generated for `Type`
/// (`#[strum_discriminants(name(TypeKind), derive(EnumIter))]`). It gives
/// two things: `TypeKind::iter()` lists every variant name (the required
/// set), and `TypeKind::from(&value)` names the variant an example is. The
/// coverage test compares the two, so adding a variant to `Type` without
/// adding an example here fails the test rather than silently shipping no
/// fixture. Examples are expressions evaluated in a `Result` context (`?`
/// allowed).
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

const T0: i64 = 1_700_000_000_000;

fn ts(offset_s: i64) -> Timestamp {
    Timestamp::from_millis(T0 + offset_s * 1000)
}

fn oid(seed: u8) -> Oid {
    let mut b = [0u8; 20];
    for (i, x) in b.iter_mut().enumerate() {
        #[allow(clippy::cast_possible_truncation)]
        let v = seed.wrapping_mul(31).wrapping_add(i as u8);
        *x = v;
    }
    Oid::from_bytes(b)
}

fn blob(seed: u8) -> BlobOid {
    BlobOid::new(oid(seed))
}
fn commit(seed: u8) -> CommitOid {
    CommitOid::new(oid(seed))
}
fn tree(seed: u8) -> TreeOid {
    TreeOid::new(oid(seed))
}
fn path(s: &str) -> Result<RepoPath, FixtureError> {
    Ok(RepoPath::new(s)?)
}
fn line(n: u32) -> Result<LineNo, FixtureError> {
    LineNo::new(n).ok_or(FixtureError::ZeroLine)
}
fn lines(a: u32, b: u32) -> Result<LineRange, FixtureError> {
    Ok(LineRange::new(line(a)?, line(b)?)?)
}
fn cols(a: u32, b: u32) -> Result<ColRange, FixtureError> {
    Ok(ColRange::new(a, b)?)
}

fn workspace_id() -> WorkspaceId {
    WorkspaceId::from_parts(1_700_000_000_000, 1)
}
fn repo_id() -> RepoId {
    RepoId::from_parts(1_700_000_000_000, 2)
}
fn repo_id_b() -> RepoId {
    RepoId::from_parts(1_700_000_000_000, 3)
}
fn review_id() -> ReviewId {
    ReviewId::from_parts(1_700_000_001_000, 4)
}
fn comment_id() -> CommentId {
    CommentId::from_parts(1_700_000_002_000, 5)
}
fn reply_id() -> CommentId {
    CommentId::from_parts(1_700_000_003_000, 6)
}
fn thread_id() -> ThreadId {
    ThreadId::from_parts(1_700_000_002_000, 5)
}
fn client_id() -> ClientId {
    ClientId::from_parts(1_700_000_000_000, 7)
}

fn human() -> Human {
    Human {
        name: "ada".into(),
        machine: "laptop".into(),
    }
}

fn human_author() -> Author {
    Author::human(human())
}

fn agent_author() -> Author {
    Author::Agent {
        name: "claude-code".into(),
        model: "claude-fable-5".into(),
        session_id: "sess_01".into(),
        invoked_by: Some(human()),
        via: AgentVia::Mcp,
    }
}

fn repo() -> Repo {
    Repo {
        id: repo_id(),
        path: "/home/ada/src/nits".into(),
        display_name: "nits".into(),
    }
}

fn repo_b() -> Repo {
    Repo {
        id: repo_id_b(),
        path: "/home/ada/src/envio".into(),
        display_name: "envio".into(),
    }
}

fn workspace() -> Workspace {
    Workspace {
        id: workspace_id(),
        name: "hacks".into(),
        repos: vec![repo(), repo_b()],
    }
}

fn sig(offset_s: i64) -> Sig {
    Sig {
        name: "Ada Lovelace".into(),
        email: "ada@example.com".into(),
        time: ts(offset_s),
        offset_minutes: 120,
    }
}

fn commit_info() -> CommitInfo {
    CommitInfo {
        oid: commit(10),
        parents: vec![commit(9)],
        tree: tree(20),
        author: sig(0),
        committer: sig(60),
        subject: "Add README, fix section reference".into(),
        body: "Longer explanation.\n\nWith paragraphs.".into(),
    }
}

fn targets() -> Result<NonEmpty<ReviewTarget>, FixtureError> {
    Ok(NonEmpty::new(vec![
        ReviewTarget {
            repo_id: repo_id(),
            base: RefSpec::Branch {
                name: "main".into(),
            },
            head: RefSpec::WorkingTree,
        },
        ReviewTarget {
            repo_id: repo_id_b(),
            base: RefSpec::Commit { oid: commit(1) },
            head: RefSpec::Head,
        },
    ])?)
}

fn resolved_targets() -> Result<NonEmpty<ResolvedTarget>, FixtureError> {
    Ok(NonEmpty::new(vec![
        ResolvedTarget {
            repo_id: repo_id(),
            base: ResolvedRef {
                tree: tree(21),
                source: ResolvedSource::Commit { oid: commit(1) },
            },
            head: ResolvedRef {
                tree: tree(22),
                source: ResolvedSource::WorkingTree {
                    dirty: vec![path("src/lib.rs")?],
                    branch: Some("feature".into()),
                },
            },
        },
        ResolvedTarget {
            repo_id: repo_id_b(),
            base: ResolvedRef {
                tree: tree(23),
                source: ResolvedSource::Commit { oid: commit(1) },
            },
            head: ResolvedRef {
                tree: tree(24),
                source: ResolvedSource::Commit { oid: commit(2) },
            },
        },
    ])?)
}

fn review() -> Result<Review, FixtureError> {
    Ok(Review {
        id: review_id(),
        workspace_id: workspace_id(),
        title: "Keyboard-first client core".into(),
        targets: targets()?,
        created: ts(1),
        status: ReviewStatus::Open,
    })
}

fn lines_anchor() -> Result<Anchor, FixtureError> {
    Ok(Anchor::Lines {
        repo_id: repo_id(),
        path: path("src/lib.rs")?,
        side: Side::Head,
        blob_oid: blob(30),
        lines: lines(10, 12)?,
        context_hash: ContextHash::new(0x0123_4567_89ab_cdef),
    })
}

fn file_anchor() -> Result<Anchor, FixtureError> {
    Ok(Anchor::File {
        repo_id: repo_id(),
        path: path("README.md")?,
        blob_oid: blob(31),
    })
}

fn comment() -> Result<Comment, FixtureError> {
    Ok(Comment {
        id: comment_id(),
        review_id: review_id(),
        thread_id: thread_id(),
        author: human_author(),
        kind: CommentKind::Note,
        anchor: lines_anchor()?,
        body: "This should be a newtype.".into(),
        created: ts(2),
        edited: None,
        state: CommentState::Live,
        context: Some(CommentContext::Diff {
            change: ChangeKind::Modified {
                old: blob(30),
                new: blob(31),
            },
        }),
    })
}

fn reply_comment() -> Result<Comment, FixtureError> {
    Ok(Comment {
        id: reply_id(),
        review_id: review_id(),
        thread_id: thread_id(),
        author: agent_author(),
        kind: CommentKind::Suggestion {
            patch: "@@ -10,1 +10,1 @@\n-let x = 1;\n+let x: u32 = 1;\n".into(),
        },
        anchor: lines_anchor()?,
        body: "Agreed; suggested change attached.".into(),
        created: ts(3),
        edited: Some(ts(4)),
        state: CommentState::Live,
        context: None,
    })
}

fn thread() -> Thread {
    Thread {
        id: thread_id(),
        review_id: review_id(),
        root: comment_id(),
        replies: vec![reply_id()],
        resolution: ThreadResolution::Open,
    }
}

fn viewed_mark() -> Result<ViewedMark, FixtureError> {
    Ok(ViewedMark {
        review_id: review_id(),
        repo_id: repo_id(),
        path: path("src/lib.rs")?,
        viewer: human(),
        blob_oid: Some(blob(30)),
    })
}

fn modified() -> ChangeKind {
    ChangeKind::Modified {
        old: blob(29),
        new: blob(30),
    }
}

fn file_entry(p: &str, seed: u8, size: u64) -> Result<TreeEntry, FixtureError> {
    Ok(TreeEntry {
        path: path(p)?,
        kind: TreeEntryKind::File {
            oid: blob(seed),
            size,
            executable: false,
        },
    })
}

fn tree_snapshot() -> Result<TreeSnapshot, FixtureError> {
    Ok(TreeSnapshot {
        repo_id: repo_id(),
        root_oid: tree(22),
        entries: vec![
            file_entry("README.md", 31, 1267)?,
            TreeEntry {
                path: path("src")?,
                kind: TreeEntryKind::Dir { oid: tree(40) },
            },
            file_entry("src/lib.rs", 30, 4096)?,
        ],
    })
}

fn cell(n: u32, text: &str) -> Result<Cell, FixtureError> {
    Ok(Cell {
        line_no: line(n)?,
        text: text.into(),
        spans: vec![Span {
            range: cols(0, 3)?,
            class: SpanClass::Keyword,
        }],
        changed: vec![],
    })
}

fn render_header() -> Result<FileRenderHeader, FixtureError> {
    Ok(FileRenderHeader {
        repo_id: repo_id(),
        path: path("src/lib.rs")?,
        target: RenderTarget::Diff { change: modified() },
        opts: RenderOpts::default(),
        lang: Some("Rust".into()),
        content: RenderContent::Text {
            total_rows: 1203,
            chunk_rows: 500,
            chunk_count: 3,
            highlighted: true,
            additions: 12,
            deletions: 4,
            gaps: GapTable::try_from(vec![GapRow {
                gap: Gap::new(1),
                row: 40,
            }])?,
        },
    })
}

fn render_chunk() -> Result<RenderChunk, FixtureError> {
    Ok(RenderChunk {
        index: ChunkIndex::FIRST,
        rows: vec![
            Row::HunkHeader {
                text: "@@ -8,4 +8,5 @@ fn main()".into(),
            },
            Row::Context {
                left: cell(8, "fn main() {")?,
                right: cell(8, "fn main() {")?,
            },
            Row::Modified {
                left: Cell {
                    changed: vec![cols(8, 9)?],
                    ..cell(9, "    let x = 1;")?
                },
                right: Cell {
                    changed: vec![cols(8, 14)?],
                    ..cell(9, "    let x: u32 = 1;")?
                },
            },
            Row::Added {
                right: cell(10, "    let y = 2;")?,
            },
            Row::Expander {
                hidden: 40,
                dir: ExpandDir::Down,
                gap: Gap::new(1),
            },
        ],
    })
}

fn event(body: EventBody) -> Event {
    Event {
        seq: Seq::new(42),
        ts: ts(5),
        author: human_author(),
        client_id: client_id(),
        client_seq: ClientSeq::new(7),
        body,
    }
}

fn review_request() -> ReviewRequest {
    ReviewRequest {
        id: ReviewRequestId::from_event_seq(Seq::new(40)),
        review_id: review_id(),
        requester: human_author(),
        recipient: "review-agent".into(),
        note: "Please review the updated parser.".into(),
        created: Timestamp::from_millis(1_700_000_000_000),
    }
}

fn review_snapshot() -> Result<ReviewSnapshot, FixtureError> {
    Ok(ReviewSnapshot {
        review: review()?,
        resolved: Some(resolved_targets()?),
        threads: vec![thread()],
        comments: vec![comment()?, reply_comment()?],
        viewed: vec![viewed_mark()?],
        requests: vec![review_request()],
        seq: Seq::new(42),
    })
}

// ---------------------------------------------------------------------------
// Registrations
// ---------------------------------------------------------------------------

struct_fixture!(Workspace, "Workspace", workspace());
struct_fixture!(Repo, "Repo", repo());
enum_fixture!(
    RefSpec,
    RefSpecKind,
    "RefSpec",
    [
        RefSpec::Branch {
            name: "main".into()
        },
        RefSpec::Commit { oid: commit(1) },
        RefSpec::Tag {
            name: "v0.1.0".into()
        },
        RefSpec::WorkingTree,
        RefSpec::Upstream,
        RefSpec::Head,
    ]
);
enum_fixture!(
    BaseRefSpec,
    BaseRefSpecKind,
    "BaseRefSpec",
    [
        BaseRefSpec::Branch {
            name: "main".into()
        },
        BaseRefSpec::Commit { oid: commit(1) },
        BaseRefSpec::Tag {
            name: "v0.1.0".into()
        },
        BaseRefSpec::Upstream,
        BaseRefSpec::Head,
    ]
);
enum_fixture!(
    TargetRevision,
    TargetRevisionKind,
    "TargetRevision",
    [
        TargetRevision::Base {
            ref_spec: BaseRefSpec::Branch {
                name: "main".into()
            }
        },
        TargetRevision::Head {
            ref_spec: RefSpec::WorkingTree
        },
    ]
);
struct_fixture!(
    ReviewTargetUpdate,
    "ReviewTargetUpdate",
    ReviewTargetUpdate {
        repo_id: repo_id(),
        revision: TargetRevision::Head {
            ref_spec: RefSpec::WorkingTree
        }
    }
);
struct_fixture!(
    RefCandidate,
    "RefCandidate",
    RefCandidate {
        ref_spec: RefSpec::Commit { oid: commit(1) },
        subject: Some("Keep selectors typed".into())
    }
);
struct_fixture!(
    ResolvedRef,
    "ResolvedRef",
    resolved_targets()?.first().head.clone()
);
enum_fixture!(
    ResolvedSource,
    ResolvedSourceKind,
    "ResolvedSource",
    [
        ResolvedSource::Commit { oid: commit(1) },
        ResolvedSource::WorkingTree {
            dirty: vec![path("src/lib.rs")?, path("new.txt")?],
            branch: Some("feature".into()),
        },
    ]
);
struct_fixture!(ReviewTarget, "ReviewTarget", targets()?.first().clone());
struct_fixture!(
    ResolvedTarget,
    "ResolvedTarget",
    resolved_targets()?.first().clone()
);
unit_enum_fixture!(ReviewStatus, "ReviewStatus");
struct_fixture!(Review, "Review", review()?);
struct_fixture!(Sig, "Sig", sig(0));
struct_fixture!(CommitInfo, "CommitInfo", commit_info());
struct_fixture!(Human, "Human", human());
unit_enum_fixture!(AgentVia, "AgentVia");
enum_fixture!(
    Author,
    AuthorKind,
    "Author",
    [
        human_author(),
        agent_author(),
        Author::Daemon {
            machine: "workstation".into()
        }
    ]
);
unit_enum_fixture!(Side, "Side");
unit_enum_fixture!(CommentIntent, "CommentIntent");
enum_fixture!(
    Anchor,
    AnchorKind,
    "Anchor",
    [Anchor::Review, file_anchor()?, lines_anchor()?]
);
enum_fixture!(
    CommentKind,
    CommentKindKind,
    "CommentKind",
    [
        CommentKind::Note,
        CommentKind::Informational,
        CommentKind::Suggestion {
            patch: "@@ -1 +1 @@\n-a\n+b\n".into()
        },
        CommentKind::Request,
    ]
);
enum_fixture!(
    CommentContext,
    CommentContextKind,
    "CommentContext",
    [
        CommentContext::Diff { change: modified() },
        CommentContext::Browse {
            reference: RefSpec::WorkingTree
        }
    ]
);
enum_fixture!(
    CommentState,
    CommentStateKind,
    "CommentState",
    [
        CommentState::Live,
        CommentState::Outdated {
            last_good_anchor: lines_anchor()?
        },
        CommentState::Deleted,
    ]
);
struct_fixture!(Comment, "Comment", comment()?);
enum_fixture!(
    ThreadResolution,
    ThreadResolutionKind,
    "ThreadResolution",
    [
        ThreadResolution::Open,
        ThreadResolution::Informational,
        ThreadResolution::Deferred {
            reason: "Deferred to controller issue per scope decision"
                .parse()
                .map_err(FixtureError::Invalid)?,
            tracking_url: Some(
                "https://example.com/issues/288"
                    .parse()
                    .map_err(FixtureError::Invalid)?
            ),
            by: human_author(),
            at: ts(9)
        },
        ThreadResolution::Resolved {
            by: human_author(),
            at: ts(9)
        },
    ]
);
struct_fixture!(Thread, "Thread", thread());
struct_fixture!(ViewedMark, "ViewedMark", viewed_mark()?);
struct_fixture!(
    GapRow,
    "GapRow",
    GapRow {
        gap: Gap::new(1),
        row: 40
    }
);
struct_fixture!(
    GapExpansion,
    "GapExpansion",
    GapExpansion {
        gap: Gap::new(1),
        up: 20,
        down: 0
    }
);
struct_fixture!(
    RenderOpts,
    "RenderOpts",
    RenderOpts {
        ignore_whitespace: true,
        context_lines: 5,
        // One opened gap, so the boundary proves the shape both ways.
        expanded: Expansions::default().opened(Gap::new(1), ExpandDir::Up, 20)
    }
);
enum_fixture!(
    DiffScope,
    DiffScopeKind,
    "DiffScope",
    [
        DiffScope::All,
        DiffScope::Committed,
        DiffScope::Commit {
            repo_id: repo_id(),
            oid: commit(11)
        },
        DiffScope::Worktree { repo_id: repo_id() },
    ]
);
enum_fixture!(
    ChangeKind,
    ChangeKindKind,
    "ChangeKind",
    [
        ChangeKind::Added { new: blob(30) },
        ChangeKind::Deleted { old: blob(29) },
        modified(),
        ChangeKind::Renamed {
            from: path("src/old.rs")?,
            old: blob(29),
            new: blob(30)
        },
    ]
);
struct_fixture!(
    ContentHit,
    "ContentHit",
    ContentHit {
        repo_id: repo_id(),
        path: path("src/lib.rs")?,
        line: LineNo::new(14).ok_or(FixtureError::ZeroLine)?,
        text: "    let total = 14;".into()
    }
);
struct_fixture!(
    FileChange,
    "FileChange",
    FileChange {
        repo_id: repo_id(),
        path: path("src/lib.rs")?,
        kind: modified()
    }
);
enum_fixture!(
    TreeEntryKind,
    TreeEntryKindKind,
    "TreeEntryKind",
    [
        TreeEntryKind::File {
            oid: blob(30),
            size: 4096,
            executable: true
        },
        TreeEntryKind::Dir { oid: tree(40) },
        TreeEntryKind::Symlink { oid: blob(32) },
        TreeEntryKind::Submodule { commit: commit(3) },
    ]
);
struct_fixture!(TreeEntry, "TreeEntry", file_entry("src/lib.rs", 30, 4096)?);
struct_fixture!(TreeSnapshot, "TreeSnapshot", tree_snapshot()?);
struct_fixture!(
    TreeDelta,
    "TreeDelta",
    TreeDelta {
        repo_id: repo_id(),
        from_root: tree(22),
        to_root: tree(25),
        added: vec![file_entry("new.txt", 33, 12)?],
        removed: vec![path("README.md")?],
        changed: vec![file_entry("src/lib.rs", 34, 4100)?],
    }
);

struct_fixture!(
    Event,
    "Event",
    event(EventBody::CommentCreated {
        comment: comment()?
    })
);
enum_fixture!(
    EventBody,
    EventKind,
    "EventBody",
    [
        EventBody::WorkspaceCreated {
            workspace: workspace()
        },
        EventBody::WorkspaceUpdated {
            workspace_id: workspace_id(),
            name: "hacks-2".into()
        },
        EventBody::RepoAttached {
            workspace_id: workspace_id(),
            repo: repo_b()
        },
        EventBody::RepoDetached {
            workspace_id: workspace_id(),
            repo_id: repo_id_b()
        },
        EventBody::ReviewCreated { review: review()? },
        EventBody::ReviewUpdated {
            review_id: review_id(),
            title: "Renamed".into(),
            status: ReviewStatus::Archived
        },
        EventBody::ReviewTargetUpdated {
            review_id: review_id(),
            target: targets()?.first().clone()
        },
        EventBody::ReviewDeleted {
            review_id: review_id()
        },
        EventBody::ReviewTargetsResolved {
            review_id: review_id(),
            targets: resolved_targets()?
        },
        EventBody::CommentCreated {
            comment: comment()?
        },
        EventBody::CommentEdited {
            review_id: review_id(),
            comment_id: comment_id(),
            body: "Edited body.".into()
        },
        EventBody::CommentDeleted {
            review_id: review_id(),
            comment_id: comment_id()
        },
        EventBody::CommentReanchored {
            review_id: review_id(),
            comment_id: comment_id(),
            anchor: lines_anchor()?,
            state: CommentState::Outdated {
                last_good_anchor: lines_anchor()?
            },
        },
        EventBody::ThreadDeferred {
            review_id: review_id(),
            thread_id: thread_id(),
            reason: "Deferred to controller issue per scope decision"
                .parse()
                .map_err(FixtureError::Invalid)?,
            tracking_url: Some(
                "https://example.com/issues/288"
                    .parse()
                    .map_err(FixtureError::Invalid)?
            )
        },
        EventBody::ThreadResolved {
            review_id: review_id(),
            thread_id: thread_id()
        },
        EventBody::ThreadUnresolved {
            review_id: review_id(),
            thread_id: thread_id()
        },
        EventBody::FileViewed {
            review_id: review_id(),
            repo_id: repo_id(),
            path: path("src/lib.rs")?,
            viewer: human(),
            blob_oid: Some(blob(30))
        },
        EventBody::FileUnviewed {
            review_id: review_id(),
            repo_id: repo_id(),
            path: path("src/lib.rs")?,
            viewer: human()
        },
        EventBody::ReviewRequested {
            review_id: review_id(),
            agent: "claude-code".into(),
            note: "Please review the store.".into()
        },
        EventBody::SuggestionApplied {
            review_id: review_id(),
            comment_id: reply_id(),
            repo_id: repo_id(),
            path: path("src/lib.rs")?,
            result_blob: blob(35)
        },
    ]
);

unit_enum_fixture!(SpanClass, "SpanClass");
struct_fixture!(
    Span,
    "Span",
    Span {
        range: cols(4, 9)?,
        class: SpanClass::Function
    }
);
struct_fixture!(
    Cell,
    "Cell",
    Cell {
        changed: vec![cols(8, 9)?],
        ..cell(9, "    let x = 1;")?
    }
);
unit_enum_fixture!(ExpandDir, "ExpandDir");
enum_fixture!(
    Row,
    RowKind,
    "Row",
    [
        Row::HunkHeader {
            text: "@@ -8,4 +8,5 @@ fn main()".into()
        },
        Row::Context {
            left: cell(8, "fn main() {")?,
            right: cell(8, "fn main() {")?
        },
        Row::Removed {
            left: cell(9, "    let x = 1;")?
        },
        Row::Added {
            right: cell(10, "    let y = 2;")?
        },
        Row::Modified {
            left: Cell {
                changed: vec![cols(8, 9)?],
                ..cell(9, "    let x = 1;")?
            },
            right: Cell {
                changed: vec![cols(8, 14)?],
                ..cell(9, "    let x: u32 = 1;")?
            },
        },
        Row::Expander {
            hidden: 40,
            dir: ExpandDir::Both,
            gap: Gap::new(1)
        },
        Row::WhitespaceOnly,
    ]
);
enum_fixture!(
    RenderTarget,
    RenderTargetKind,
    "RenderTarget",
    [
        RenderTarget::Diff { change: modified() },
        RenderTarget::Blob { oid: blob(30) },
    ]
);
enum_fixture!(
    RenderContent,
    RenderContentKind,
    "RenderContent",
    [
        RenderContent::Binary,
        RenderContent::Text {
            total_rows: 1203,
            chunk_rows: 500,
            chunk_count: 3,
            highlighted: true,
            additions: 12,
            deletions: 4,
            gaps: GapTable::try_from(vec![GapRow {
                gap: Gap::new(1),
                row: 40,
            }])?
        },
    ]
);
struct_fixture!(FileRenderHeader, "FileRenderHeader", render_header()?);
struct_fixture!(RenderChunk, "RenderChunk", render_chunk()?);
struct_fixture!(
    FileRender,
    "FileRender",
    FileRender {
        header: render_header()?,
        chunks: vec![render_chunk()?]
    }
);
struct_fixture!(
    FileSummary,
    "FileSummary",
    FileSummary {
        repo_id: repo_id(),
        path: path("src/lib.rs")?,
        change: modified(),
        additions: 12,
        deletions: 4,
        binary: false,
    }
);
struct_fixture!(
    DiffSummary,
    "DiffSummary",
    DiffSummary {
        files: vec![FileSummary {
            repo_id: repo_id(),
            path: path("src/lib.rs")?,
            change: modified(),
            additions: 12,
            deletions: 4,
            binary: false
        }],
        additions: 12,
        deletions: 4,
    }
);

enum_fixture!(
    ClientMsg,
    ClientMsgKind,
    "ClientMsg",
    [
        ClientMsg::Hello {
            client_id: client_id(),
            protocol: ProtocolVersion::CURRENT,
            client: client_build(),
            author: human_author(),
        },
        ClientMsg::Request {
            id: RequestId::new(1),
            request: Request::OpenReview {
                review_id: review_id(),
                opts: RenderOpts::default()
            }
        },
        ClientMsg::Cancel {
            id: RequestId::new(1)
        },
    ]
);
enum_fixture!(
    ServerMsg,
    ServerMsgKind,
    "ServerMsg",
    [
        ServerMsg::Welcome {
            protocol: ProtocolVersion::CURRENT,
            daemon: daemon_build(),
            schema: SchemaVersion::CURRENT,
            upgrade: None,
        },
        ServerMsg::Rejected {
            error: RpcError::UnsupportedProtocol {
                requested: ProtocolVersion::new(2, 0, 0),
                supported: vec![ProtocolVersion::CURRENT],
            },
        },
        ServerMsg::Response {
            id: RequestId::new(1),
            response: Response::Subscribed { seq: Seq::new(42) }
        },
        ServerMsg::StreamItem {
            id: RequestId::new(2),
            item: StreamItem::Header {
                header: render_header()?
            }
        },
        ServerMsg::StreamEnd {
            id: RequestId::new(2)
        },
        ServerMsg::Error {
            id: RequestId::new(3),
            error: RpcError::NotFound {
                kind: EntityKind::Review,
                id: review_id().to_string()
            }
        },
        ServerMsg::Event {
            event: event(EventBody::ThreadResolved {
                review_id: review_id(),
                thread_id: thread_id()
            })
        },
        ServerMsg::TreeDelta {
            delta: TreeDelta {
                repo_id: repo_id(),
                from_root: tree(22),
                to_root: tree(25),
                added: vec![],
                removed: vec![],
                changed: vec![file_entry("src/lib.rs", 34, 4100)?],
            }
        },
    ]
);
enum_fixture!(
    Since,
    SinceKind,
    "Since",
    [Since::Now, Since::After { seq: Seq::new(42) }]
);
enum_fixture!(
    SubscribeScope,
    SubscribeScopeKind,
    "SubscribeScope",
    [
        SubscribeScope::All,
        SubscribeScope::Workspace {
            workspace_id: workspace_id()
        },
        SubscribeScope::Review {
            review_id: review_id()
        },
        SubscribeScope::AwaitingAgent {
            agent: "claude-code".into()
        },
    ]
);
enum_fixture!(
    Mutation,
    MutationKind,
    "Mutation",
    [
        Mutation::CreateWorkspace {
            workspace_id: workspace_id(),
            name: "hacks".into()
        },
        Mutation::RenameWorkspace {
            workspace_id: workspace_id(),
            name: "hacks-2".into()
        },
        Mutation::AttachRepo {
            workspace_id: workspace_id(),
            repo_id: repo_id(),
            path: "/home/ada/src/nits".into(),
            display_name: "nits".into()
        },
        Mutation::DetachRepo {
            workspace_id: workspace_id(),
            repo_id: repo_id()
        },
        Mutation::CreateReview {
            review_id: review_id(),
            workspace_id: workspace_id(),
            title: "Keyboard-first client core".into(),
            targets: targets()?
        },
        Mutation::UpdateReview {
            review_id: review_id(),
            title: "Renamed".into(),
            status: ReviewStatus::Archived
        },
        Mutation::UpdateReviewTarget {
            review_id: review_id(),
            update: ReviewTargetUpdate {
                repo_id: repo_id(),
                revision: TargetRevision::Head {
                    ref_spec: RefSpec::WorkingTree
                }
            }
        },
        Mutation::DeleteReview {
            review_id: review_id()
        },
        Mutation::AddComment {
            review_id: review_id(),
            comment_id: comment_id(),
            kind: CommentKind::Note,
            anchor: lines_anchor()?,
            context: None,
            body: "This should be a newtype.".into()
        },
        Mutation::Reply {
            review_id: review_id(),
            thread_id: thread_id(),
            comment_id: reply_id(),
            kind: CommentKind::Note,
            body: "Agreed.".into()
        },
        Mutation::EditComment {
            review_id: review_id(),
            comment_id: comment_id(),
            body: "Edited body.".into()
        },
        Mutation::DeleteComment {
            review_id: review_id(),
            comment_id: comment_id()
        },
        Mutation::DeferThread {
            review_id: review_id(),
            thread_id: thread_id(),
            reason: "Deferred to controller issue per scope decision"
                .parse()
                .map_err(FixtureError::Invalid)?,
            tracking_url: Some(
                "https://example.com/issues/288"
                    .parse()
                    .map_err(FixtureError::Invalid)?
            )
        },
        Mutation::ResolveThread {
            review_id: review_id(),
            thread_id: thread_id()
        },
        Mutation::UnresolveThread {
            review_id: review_id(),
            thread_id: thread_id()
        },
        Mutation::MarkViewed {
            review_id: review_id(),
            repo_id: repo_id(),
            path: path("src/lib.rs")?
        },
        Mutation::UnmarkViewed {
            review_id: review_id(),
            repo_id: repo_id(),
            path: path("src/lib.rs")?
        },
        Mutation::RequestReview {
            review_id: review_id(),
            agent: "claude-code".into(),
            note: "Please review the store.".into()
        },
        Mutation::ApplySuggestion {
            review_id: review_id(),
            comment_id: reply_id()
        },
    ]
);
enum_fixture!(
    Request,
    RequestKind,
    "Request",
    [
        Request::EnsureDirectoryReview {
            client_seq: ClientSeq::new(7),
            options: directory_options()
        },
        Request::ListWorkspaces,
        Request::ListReviews {
            workspace_id: workspace_id()
        },
        Request::ListRefs { repo_id: repo_id() },
        Request::DefaultBase { repo_id: repo_id() },
        Request::GetReview {
            review_id: review_id()
        },
        Request::ReviewSnapshot {
            review_id: review_id()
        },
        Request::ListFiles {
            review_id: review_id(),
            scope: DiffScope::All
        },
        Request::OpenReview {
            review_id: review_id(),
            opts: RenderOpts::default()
        },
        Request::Search {
            review_id: review_id(),
            query: "todo".into(),
            all_files: true,
            scope: DiffScope::All
        },
        Request::ResolveTargets {
            review_id: review_id()
        },
        Request::ListCommits {
            review_id: review_id(),
            repo_id: repo_id()
        },
        Request::TreeSnapshot {
            repo_id: repo_id(),
            ref_spec: RefSpec::Branch {
                name: "main".into()
            }
        },
        Request::FileRender {
            review_id: review_id(),
            repo_id: repo_id(),
            path: path("src/lib.rs")?,
            opts: RenderOpts::default(),
            first_chunk: ChunkIndex::new(2),
            scope: DiffScope::Committed
        },
        Request::ChangeRender {
            repo_id: repo_id(),
            path: path("src/lib.rs")?,
            change: modified(),
            opts: RenderOpts::default(),
            first_chunk: ChunkIndex::FIRST
        },
        Request::BlobRender {
            repo_id: repo_id(),
            path: path("src/lib.rs")?,
            blob_oid: blob(30),
            first_chunk: ChunkIndex::FIRST
        },
        Request::RenderChunk {
            repo_id: repo_id(),
            path: path("src/lib.rs")?,
            target: RenderTarget::Diff { change: modified() },
            opts: RenderOpts::default(),
            index: ChunkIndex::new(1)
        },
        Request::Subscribe {
            scope: SubscribeScope::Review {
                review_id: review_id()
            },
            since: Since::After { seq: Seq::new(41) }
        },
        Request::Shutdown,
        Request::Unsubscribe {
            scope: SubscribeScope::Review {
                review_id: review_id()
            }
        },
        Request::Mutate {
            client_seq: ClientSeq::new(7),
            mutation: Mutation::ResolveThread {
                review_id: review_id(),
                thread_id: thread_id()
            }
        },
    ]
);
struct_fixture!(
    ReviewRequestId,
    "ReviewRequestId",
    ReviewRequestId::from_event_seq(Seq::new(40))
);
struct_fixture!(ReviewRequest, "ReviewRequest", review_request());
struct_fixture!(ReviewSnapshot, "ReviewSnapshot", review_snapshot()?);
enum_fixture!(
    Response,
    ResponseKind,
    "Response",
    [
        Response::DirectoryReview {
            review: directory_review()
        },
        Response::Workspaces {
            workspaces: vec![workspace()]
        },
        Response::Reviews {
            reviews: vec![review()?]
        },
        Response::DefaultBase {
            base: RefSpec::Branch {
                name: "main".into()
            }
        },
        Response::Review { review: review()? },
        Response::ReviewSnapshot {
            snapshot: review_snapshot()?
        },
        Response::Files {
            files: vec![FileChange {
                repo_id: repo_id(),
                path: path("src/lib.rs")?,
                kind: modified()
            }],
            resolved: resolved_targets()?.into_iter().collect()
        },
        Response::Search {
            hits: vec![ContentHit {
                repo_id: repo_id(),
                path: path("src/lib.rs")?,
                line: LineNo::new(14).ok_or(FixtureError::ZeroLine)?,
                text: "    let total = 14;".into()
            }],
            truncated: false
        },
        Response::Resolved {
            targets: resolved_targets()?,
            changed: true
        },
        Response::Commits {
            commits: vec![commit_info()]
        },
        Response::Refs {
            repo_id: repo_id(),
            refs: vec![RefCandidate {
                ref_spec: RefSpec::Commit { oid: commit(1) },
                subject: Some("Keep selectors typed".into())
            }]
        },
        Response::TreeSnapshot {
            snapshot: tree_snapshot()?
        },
        Response::RenderChunk {
            chunk: render_chunk()?
        },
        Response::Subscribed { seq: Seq::new(42) },
        Response::Unsubscribed,
        Response::ShuttingDown,
        Response::Committed {
            event: event(EventBody::ThreadResolved {
                review_id: review_id(),
                thread_id: thread_id()
            })
        },
    ]
);
enum_fixture!(
    StreamItem,
    StreamItemKind,
    "StreamItem",
    [
        StreamItem::ReviewSnapshot {
            snapshot: review_snapshot()?
        },
        StreamItem::TreeSnapshot {
            snapshot: tree_snapshot()?
        },
        StreamItem::Header {
            header: render_header()?
        },
        StreamItem::Chunk {
            repo_id: repo_id(),
            path: path("src/lib.rs")?,
            chunk: render_chunk()?
        },
    ]
);
unit_enum_fixture!(EntityKind, "EntityKind");
enum_fixture!(
    RpcError,
    RpcErrorKind,
    "RpcError",
    [
        RpcError::NotFound {
            kind: EntityKind::Comment,
            id: comment_id().to_string()
        },
        RpcError::Invalid {
            reason: "line range exceeds blob length".into()
        },
        RpcError::Forbidden {
            reason: "agents cannot mark files viewed".into()
        },
        RpcError::SeqTooOld {
            oldest: Seq::new(1000)
        },
        RpcError::Cancelled,
        RpcError::UnsupportedProtocol {
            requested: ProtocolVersion::new(2, 0, 0),
            supported: vec![ProtocolVersion::CURRENT]
        },
        RpcError::VersionMismatch {
            negotiated: ProtocolVersion::CURRENT,
            received: ProtocolVersion::new(0, 2, 0)
        },
        RpcError::Internal {
            message: "redb: database locked".into()
        },
    ]
);
unit_enum_fixture!(ViewSection, "ViewSection");

fn client_build() -> BuildInfo {
    BuildInfo {
        name: "nits-client-tauri".into(),
        version: "0.1.0".into(),
    }
}
fn daemon_build() -> BuildInfo {
    BuildInfo {
        name: "nitsd".into(),
        version: "0.1.0".into(),
    }
}
fn upgrade_notice() -> UpgradeNotice {
    UpgradeNotice {
        latest: ProtocolVersion::CURRENT,
        message: format!(
            "Upgrade your client to protocol {}.",
            ProtocolVersion::CURRENT
        ),
    }
}
struct_fixture!(ProtocolVersion, "ProtocolVersion", ProtocolVersion::CURRENT);
struct_fixture!(SchemaVersion, "SchemaVersion", SchemaVersion::CURRENT);
struct_fixture!(BuildInfo, "BuildInfo", daemon_build());
struct_fixture!(UpgradeNotice, "UpgradeNotice", upgrade_notice());
struct_fixture!(
    Envelope<ClientMsg>,
    "EnvelopeClientMsg",
    Envelope::current(ClientMsg::Hello {
        client_id: client_id(),
        protocol: ProtocolVersion::CURRENT,
        client: client_build(),
        author: human_author(),
    })
);
struct_fixture!(
    Envelope<ServerMsg>,
    "EnvelopeServerMsg",
    Envelope::current(ServerMsg::Welcome {
        protocol: ProtocolVersion::CURRENT,
        daemon: daemon_build(),
        schema: SchemaVersion::CURRENT,
        upgrade: None
    })
);

fn directory_options() -> EnsureDirectoryReview {
    EnsureDirectoryReview {
        workspace_id: workspace_id(),
        repo_id: repo_id(),
        review_id: review_id(),
        path: "/repos/example".into(),
        base: Some(BaseRefSpec::Head),
        head: None,
    }
}
fn directory_review() -> DirectoryReview {
    DirectoryReview {
        workspace_id: workspace_id(),
        repo_id: repo_id(),
        review_id: review_id(),
        base: RefSpec::Head,
        head: RefSpec::WorkingTree,
        outcome: DirectoryReviewOutcome::Created,
        seq: Seq::new(42),
    }
}
struct_fixture!(
    EnsureDirectoryReview,
    "EnsureDirectoryReview",
    directory_options()
);
struct_fixture!(DirectoryReview, "DirectoryReview", directory_review());
unit_enum_fixture!(DirectoryReviewOutcome, "DirectoryReviewOutcome");
