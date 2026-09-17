//! Diff, thread, conversation and commit-stepper views (plan 3.5). All
//! pure functions of the open review, the cache and the viewport; derived
//! after every input like the explorer.
//!
//! Comment placement (§6.5): a `Lines` anchor lands on the last row whose
//! cell on the anchored side is inside the range *and* whose blob matches
//! the render target; a `File` anchor lands on row 0; `Review` anchors go
//! to the conversation panel. Outdated comments are placed by their last
//! good anchor and flagged; deleted ones are not placed at all.

use nits_protocol::{
    Anchor, Author, BlobOid, ChunkIndex, Comment, CommentId, CommentKind, CommentState, CommitInfo,
    CommitOid, FileRenderHeader, RenderChunk, RenderContent, RenderTarget, RepoId, ReviewSnapshot,
    Row, Side, Thread, ThreadId, ThreadResolution, Timestamp,
};
use serde::{Deserialize, Serialize};
use strum::EnumDiscriminants;

use crate::cache::{CacheKey, CacheValue, ContentCache, RenderKey};
use crate::content::FileRef;
use crate::explorer::ViewedState;

/// Where a thread is anchored, for the thread list and the diff overlay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, EnumDiscriminants)]
#[strum_discriminants(name(ThreadPlaceKind), derive(Hash, strum::EnumIter))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum ThreadPlace {
    Review,
    File {
        file: FileRef,
    },
    Lines {
        file: FileRef,
        side: Side,
        /// 1-based first and last line.
        start: u32,
        end: u32,
    },
}

/// Actionable status, kept separate from location and comment content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::EnumIter)]
pub enum ThreadStatus {
    Open,
    Resolved,
    Informational,
}

/// One thread as the thread list shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThreadView {
    pub id: ThreadId,
    pub root: CommentId,
    pub author: Author,
    pub created: Timestamp,
    /// First line of the root comment.
    pub summary: String,
    pub replies: u32,
    pub status: ThreadStatus,
    pub place: ThreadPlace,
    /// The root could not be re-anchored after the head moved.
    pub outdated: bool,
    /// Some comment in the thread is still awaiting the daemon.
    pub pending: bool,
    /// The root is a `CommentKind::Suggestion` (a patch that can be applied).
    pub suggestion: bool,
    /// Root then replies, oldest first; deleted comments are omitted.
    pub comments: Vec<CommentView>,
    /// The diff or Browse revision the root was made on, when recorded — the
    /// target of jump-to-original-diff (UI-DESIGN §Comments).
    #[serde(default)]
    pub context: Option<nits_protocol::CommentContext>,
}

/// One comment as the thread panel shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommentView {
    pub id: CommentId,
    pub author: Author,
    pub created: Timestamp,
    pub body: String,
    /// Still awaiting the daemon (optimistic).
    pub pending: bool,
}

/// What a row is to an anchored range: the last line, under which the
/// thread's card (or the composer) renders, or a line inside the range.
/// A single-line anchor is `Anchor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::EnumIter)]
pub enum RowPlace {
    Anchor,
    Inside,
}

/// A thread placed on a row: the half of the row it is anchored to — a
/// modified row shows base threads against its removed cell and head
/// threads against its added one — and whether this row is the range's
/// last line or one inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RowThread {
    pub thread: ThreadId,
    pub side: Side,
    pub place: RowPlace,
}

/// A row of the open file with the threads placed on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffRow {
    /// Index into the file's rows (across chunks).
    pub index: u32,
    pub row: Row,
    pub threads: Vec<RowThread>,
    /// The open draft's range covers this row. The composer renders
    /// under the `Anchor` row, so a comment is written where it will be
    /// read (UI-DESIGN §Comments) — mapping the draft's anchor to rows is
    /// anchoring work, which lives here and not in a component.
    #[serde(default)]
    pub drafted: Option<(RowPlace, Side)>,
}

/// The open file, over the viewport window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiffView {
    /// Content identity for clearing host row caches when a ref moves.
    pub target: RenderTarget,
    pub file: FileRef,
    pub lang: Option<String>,
    pub content: RenderContent,
    /// Whether the current viewer marked this file viewed at its head blob;
    /// hosts collapse `Viewed` files.
    pub viewed: ViewedState,
    pub first_row: u32,
    pub last_row: u32,
    /// Rows of the window that are cached, in order; gaps are chunks still
    /// on their way (`missing`).
    pub rows: Vec<DiffRow>,
    pub missing: Vec<ChunkIndex>,
    /// Threads anchored to the whole file (shown above the rows).
    pub file_threads: Vec<ThreadId>,
    /// Folded in the stacked view (`z a`); motions skip collapsed files.
    pub collapsed: bool,
    /// This is a comment's recorded original diff, opened read-only
    /// (UI-DESIGN §Comments); the UI shows the jump-to-context banner.
    #[serde(default)]
    pub original: bool,
}

/// One commit of the stepper, with what the commit panel shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepperCommit {
    pub oid: CommitOid,
    pub parents: Vec<CommitOid>,
    pub subject: String,
    /// Everything after the subject; empty if none.
    pub body: String,
    pub author: String,
    pub time: Timestamp,
    pub committer: String,
    pub committer_time: Timestamp,
}

/// Commits of one repo of the review, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitStepper {
    pub repo_id: RepoId,
    pub commits: Vec<StepperCommit>,
    /// Whether this review has the working-tree step after its commits.
    pub has_worktree: bool,
}

impl CommitStepper {
    pub(crate) fn from_commits(
        repo_id: RepoId,
        commits: &[CommitInfo],
        has_worktree: bool,
    ) -> Self {
        Self {
            repo_id,
            commits: commits
                .iter()
                .map(|c| StepperCommit {
                    oid: c.oid,
                    parents: c.parents.clone(),
                    subject: c.subject.clone(),
                    body: c.body.clone(),
                    author: c.author.name.clone(),
                    time: c.author.time,
                    committer: c.committer.name.clone(),
                    committer_time: c.committer.time,
                })
                .collect(),
            has_worktree,
        }
    }

    /// Number of selectable rows: aggregate, commits, then working tree.
    pub(crate) fn row_count(&self) -> usize {
        1 + self.commits.len() + usize::from(self.has_worktree)
    }
}

/// The comment list in thread order: threads by creation of their root,
/// each with root then replies. Deleted comments keep their slot.
#[must_use]
pub fn threads(snapshot: &ReviewSnapshot, pending: &PendingIds) -> Vec<ThreadView> {
    let mut out: Vec<ThreadView> = snapshot
        .threads
        .iter()
        .filter_map(|t| thread_view(snapshot, t, pending))
        .collect();
    out.sort_by_key(|t| t.created);
    out
}

/// What the pending mutations touch, for the `pending` marks.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PendingIds {
    pub comments: Vec<CommentId>,
    pub threads: Vec<ThreadId>,
}

fn thread_view(snapshot: &ReviewSnapshot, t: &Thread, pending: &PendingIds) -> Option<ThreadView> {
    let root = snapshot.comments.iter().find(|c| c.id == t.root)?;
    let (anchor, outdated) = placed_anchor(root)?;
    let in_thread = |id: &CommentId| *id == t.root || t.replies.contains(id);
    Some(ThreadView {
        id: t.id,
        root: root.id,
        author: root.author.clone(),
        created: root.created,
        summary: root.body.lines().next().unwrap_or_default().to_owned(),
        replies: u32::try_from(t.replies.len()).unwrap_or(u32::MAX),
        status: match t.resolution {
            ThreadResolution::Open => ThreadStatus::Open,
            ThreadResolution::Resolved { .. } => ThreadStatus::Resolved,
            ThreadResolution::Informational => ThreadStatus::Informational,
        },
        place: place_of(anchor),
        outdated,
        pending: pending.comments.iter().any(in_thread) || pending.threads.contains(&t.id),
        suggestion: matches!(root.kind, CommentKind::Suggestion { .. }),
        comments: std::iter::once(&t.root)
            .chain(t.replies.iter())
            .filter_map(|id| snapshot.comments.iter().find(|c| c.id == *id))
            .filter(|c| !matches!(c.state, CommentState::Deleted))
            .map(|c| CommentView {
                id: c.id,
                author: c.author.clone(),
                created: c.created,
                body: c.body.clone(),
                pending: pending.comments.contains(&c.id),
            })
            .collect(),
        context: root.context.clone(),
    })
}

/// Threads anchored at review level, oldest first.
#[must_use]
pub fn conversation(threads: &[ThreadView]) -> Vec<ThreadView> {
    threads
        .iter()
        .filter(|t| t.place == ThreadPlace::Review)
        .cloned()
        .collect()
}

/// The anchor a comment is shown at, and whether it is the stale one.
fn placed_anchor(c: &Comment) -> Option<(&Anchor, bool)> {
    match &c.state {
        CommentState::Live => Some((&c.anchor, false)),
        CommentState::Outdated { last_good_anchor } => Some((last_good_anchor, true)),
        CommentState::Deleted => None,
    }
}

fn place_of(anchor: &Anchor) -> ThreadPlace {
    match anchor {
        Anchor::Review => ThreadPlace::Review,
        Anchor::File { repo_id, path, .. } => ThreadPlace::File {
            file: FileRef {
                repo_id: *repo_id,
                path: path.clone(),
            },
        },
        Anchor::Lines {
            repo_id,
            path,
            side,
            lines,
            ..
        } => ThreadPlace::Lines {
            file: FileRef {
                repo_id: *repo_id,
                path: path.clone(),
            },
            side: *side,
            start: lines.start().get(),
            end: lines.end().get(),
        },
    }
}

/// Blob shown on `side` of a render target.
fn blob_on(target: &RenderTarget, side: Side) -> Option<BlobOid> {
    match target {
        RenderTarget::Diff { change } => match side {
            Side::Base => change.old_blob(),
            Side::Head => change.new_blob(),
        },
        RenderTarget::Blob { oid } => Some(*oid),
    }
}

/// Line number of `row` on `side`, if it has one.
pub(crate) fn line_on(row: &Row, side: Side) -> Option<u32> {
    match (row, side) {
        (
            Row::Context { left, .. } | Row::Modified { left, .. } | Row::Removed { left },
            Side::Base,
        ) => Some(left.line_no.get()),
        (
            Row::Context { right, .. } | Row::Modified { right, .. } | Row::Added { right },
            Side::Head,
        ) => Some(right.line_no.get()),
        (Row::Removed { .. }, Side::Head)
        | (Row::Added { .. }, Side::Base)
        | (
            Row::HunkHeader { .. } | Row::Expander { .. } | Row::WhitespaceOnly,
            Side::Base | Side::Head,
        ) => None,
    }
}

/// A thread anchored somewhere in `file`, resolved to the render target.
struct Placement {
    thread: ThreadId,
    kind: PlacementKind,
}

enum PlacementKind {
    File,
    Lines { side: Side, start: u32, end: u32 },
}

/// The open draft's line range, when it is a line anchor on this render:
/// `(side, start, end)`. A review- or file-level draft has no rows, and
/// keeps the docked composer.
fn draft_range(draft: Option<&crate::view::Draft>, render: &RenderKey) -> Option<(Side, u32, u32)> {
    let Anchor::Lines {
        repo_id,
        path,
        side,
        blob_oid,
        lines,
        ..
    } = &draft?.anchor
    else {
        return None;
    };
    if *repo_id != render.repo_id
        || *path != render.path
        || blob_on(&render.target, *side) != Some(*blob_oid)
    {
        return None;
    }
    Some((*side, lines.start().get(), lines.end().get()))
}

/// Where a row sits in `start..=end` on `side`, if at all.
fn place_in(row: &Row, side: Side, start: u32, end: u32) -> Option<RowPlace> {
    let line = line_on(row, side)?;
    if line == end {
        Some(RowPlace::Anchor)
    } else if line >= start && line < end {
        Some(RowPlace::Inside)
    } else {
        None
    }
}

fn placements(snapshot: &ReviewSnapshot, render: &RenderKey) -> Vec<Placement> {
    let mut out = Vec::new();
    for t in &snapshot.threads {
        let Some(root) = snapshot.comments.iter().find(|c| c.id == t.root) else {
            continue;
        };
        let Some((anchor, _)) = placed_anchor(root) else {
            continue;
        };
        let kind = match anchor {
            Anchor::Review => continue,
            Anchor::File {
                repo_id,
                path,
                blob_oid,
            } => {
                if *repo_id != render.repo_id || *path != render.path {
                    continue;
                }
                // A file anchor is shown if its blob is on either side.
                if blob_on(&render.target, Side::Head) != Some(*blob_oid)
                    && blob_on(&render.target, Side::Base) != Some(*blob_oid)
                {
                    continue;
                }
                PlacementKind::File
            }
            Anchor::Lines {
                repo_id,
                path,
                side,
                blob_oid,
                lines,
                ..
            } => {
                if *repo_id != render.repo_id
                    || *path != render.path
                    || blob_on(&render.target, *side) != Some(*blob_oid)
                {
                    continue;
                }
                PlacementKind::Lines {
                    side: *side,
                    start: lines.start().get(),
                    end: lines.end().get(),
                }
            }
        };
        out.push(Placement { thread: t.id, kind });
    }
    out
}

/// Every cached row of `render` with its threads, for navigation that must
/// see past the viewport (next hunk, next comment). Chunks not cached are
/// simply absent.
/// Total rows of a render whose header is cached.
pub(crate) fn total_rows_of(cache: &ContentCache, render: &RenderKey) -> Option<u32> {
    match cache.peek(&CacheKey::Header {
        render: render.clone(),
    })? {
        CacheValue::Header { header } => match header.content {
            RenderContent::Text { total_rows, .. } => Some(total_rows),
            RenderContent::Binary => None,
        },
        CacheValue::Tree { .. } | CacheValue::Chunk { .. } => None,
    }
}

pub(crate) fn all_rows(
    cache: &ContentCache,
    snapshot: &ReviewSnapshot,
    render: &RenderKey,
) -> Vec<DiffRow> {
    let Some(CacheValue::Header { header }) = cache.peek(&CacheKey::Header {
        render: render.clone(),
    }) else {
        return Vec::new();
    };
    let RenderContent::Text {
        chunk_rows,
        chunk_count,
        ..
    } = header.content
    else {
        return Vec::new();
    };
    let placements = placements(snapshot, render);
    let mut rows = Vec::new();
    for ci in 0..chunk_count {
        if let Some(CacheValue::Chunk { chunk }) = cache.peek(&CacheKey::Chunk {
            render: render.clone(),
            index: ChunkIndex::new(ci),
        }) {
            place_rows(
                chunk,
                ci * chunk_rows,
                0,
                u32::MAX,
                &placements,
                None,
                &mut rows,
            );
        }
    }
    rows
}

/// Build the diff view for `render` over rows `first_row..=last_row`.
pub(crate) fn diff_view(
    cache: &ContentCache,
    snapshot: &ReviewSnapshot,
    viewer: &Author,
    render: &RenderKey,
    first_row: u32,
    last_row: u32,
    draft: Option<&crate::view::Draft>,
) -> Option<DiffView> {
    let CacheValue::Header { header } = cache.peek(&CacheKey::Header {
        render: render.clone(),
    })?
    else {
        return None;
    };
    let file = FileRef {
        repo_id: render.repo_id,
        path: render.path.clone(),
    };
    let viewed_state = crate::explorer::viewed_state(
        snapshot,
        viewer,
        render.repo_id,
        &render.path,
        blob_on(&render.target, Side::Head),
    );
    let draft_range = draft_range(draft, render);
    let placements = placements(snapshot, render);
    let file_threads: Vec<ThreadId> = placements
        .iter()
        .filter(|p| matches!(p.kind, PlacementKind::File))
        .map(|p| p.thread)
        .collect();
    let RenderContent::Text {
        chunk_rows,
        chunk_count,
        total_rows,
        ..
    } = header.content
    else {
        return Some(DiffView {
            file,
            target: header.target.clone(),
            lang: header.lang.clone(),
            content: header.content.clone(),
            viewed: viewed_state,
            first_row,
            last_row,
            rows: Vec::new(),
            missing: Vec::new(),
            file_threads,
            collapsed: false,
            original: false,
        });
    };
    if chunk_rows == 0 || chunk_count == 0 || total_rows == 0 {
        return Some(empty(
            header,
            file,
            viewed_state,
            first_row,
            last_row,
            file_threads,
        ));
    }
    let last_row = last_row.min(total_rows - 1);
    let first_row = first_row.min(last_row);
    let first_chunk = first_row / chunk_rows;
    let last_chunk = (last_row / chunk_rows).min(chunk_count - 1);
    let mut rows = Vec::new();
    let mut missing = Vec::new();
    for ci in first_chunk..=last_chunk {
        let index = ChunkIndex::new(ci);
        match cache.peek(&CacheKey::Chunk {
            render: render.clone(),
            index,
        }) {
            Some(CacheValue::Chunk { chunk }) => {
                place_rows(
                    chunk,
                    ci * chunk_rows,
                    first_row,
                    last_row,
                    &placements,
                    draft_range,
                    &mut rows,
                );
            }
            Some(CacheValue::Tree { .. } | CacheValue::Header { .. }) | None => missing.push(index),
        }
    }
    Some(DiffView {
        file,
        target: header.target.clone(),
        lang: header.lang.clone(),
        content: header.content.clone(),
        viewed: viewed_state,
        first_row,
        last_row,
        rows,
        missing,
        file_threads,
        collapsed: false,
        original: false,
    })
}

fn empty(
    header: &FileRenderHeader,
    file: FileRef,
    viewed: ViewedState,
    first_row: u32,
    last_row: u32,
    file_threads: Vec<ThreadId>,
) -> DiffView {
    DiffView {
        file,
        target: header.target.clone(),
        lang: header.lang.clone(),
        content: header.content.clone(),
        viewed,
        first_row,
        last_row,
        rows: Vec::new(),
        missing: Vec::new(),
        file_threads,
        collapsed: false,
        original: false,
    }
}

/// Append the rows of `chunk` inside the window, each with the threads
/// whose range ends on it.
fn place_rows(
    chunk: &RenderChunk,
    base_index: u32,
    first_row: u32,
    last_row: u32,
    placements: &[Placement],
    draft: Option<(Side, u32, u32)>,
    out: &mut Vec<DiffRow>,
) {
    for (i, row) in chunk.rows.iter().enumerate() {
        let index = base_index + u32::try_from(i).unwrap_or(u32::MAX);
        if index < first_row || index > last_row {
            continue;
        }
        let mut threads = Vec::new();
        for p in placements {
            let PlacementKind::Lines { side, start, end } = p.kind else {
                continue;
            };
            // The card hangs under the range's last line; every line of
            // the range is marked, so the stretch can be drawn.
            if let Some(place) = place_in(row, side, start, end) {
                threads.push(RowThread {
                    thread: p.thread,
                    side,
                    place,
                });
            }
        }
        out.push(DiffRow {
            index,
            row: row.clone(),
            threads,
            drafted: draft.and_then(|(side, start, end)| {
                place_in(row, side, start, end).map(|place| (place, side))
            }),
        });
    }
}
