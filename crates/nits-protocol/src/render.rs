//! Diff render model: the flat list of rows the screen shows.
//!
//! Pure function of `(old blob, new blob, opts)`, produced by the daemon,
//! cached on disk, identical for every client. Comment-agnostic: overlays are
//! the client's job. See `docs/ARCHITECTURE.md` §4.6.

use serde::{Deserialize, Serialize};
use strum::{EnumDiscriminants, EnumIter};

use crate::domain::{BlobEntry, ChangeKind, RenderOpts};
use crate::ids::RepoId;
use crate::invariants::{ColRange, LineNo, RepoPath};

/// Syntax-highlight class for a span. A closed set so the UI's stylesheet
/// is exhaustive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, EnumIter)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum SpanClass {
    Keyword,
    String,
    Number,
    Comment,
    Type,
    Function,
    Variable,
    Constant,
    Operator,
    Punctuation,
    Attribute,
    Tag,
    Other,
}

/// A highlighted byte range within a cell's text.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Span {
    pub range: ColRange,
    pub class: SpanClass,
}

/// Source bytes terminating a line. Git blobs retain these bytes unchanged;
/// a trailing terminator does not create another source line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, EnumIter)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum LineEnding {
    Lf,
    CrLf,
    Missing,
}

impl LineEnding {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
            Self::Missing => "",
        }
    }
}

/// Disposable render-cache generation, shared by daemon and client storage.
/// Increment when rendering semantics change even if old headers still decode.
pub const RENDER_CACHE_GENERATION: u32 = 1;

/// One side of a row.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Cell {
    pub line_no: LineNo,
    /// Original line text without the trailing newline. Whitespace-ignored
    /// diffs still carry the real text.
    pub text: String,
    pub ending: LineEnding,
    pub spans: Vec<Span>,
    /// Intra-line changed ranges (only non-empty on `Modified` rows).
    pub changed: Vec<ColRange>,
}

/// Which direction an expander reveals hidden lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, EnumIter)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ExpandDir {
    Up,
    Down,
    Both,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(RowKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum Row {
    HunkHeader {
        text: String,
    },
    Context {
        left: Cell,
        right: Cell,
    },
    Removed {
        left: Cell,
    },
    Added {
        right: Cell,
    },
    /// A paired `-`/`+` line with intra-line ranges.
    Modified {
        left: Cell,
        right: Cell,
    },
    /// "show N more lines". `gap` names the hidden run so a client can
    /// ask for more of *this* one (`RenderOpts::expanded`).
    Expander {
        hidden: u32,
        dir: ExpandDir,
        gap: crate::domain::Gap,
    },
    /// The whole file differs only in whitespace and `ignore_whitespace` is on.
    WhitespaceOnly,
}

/// Index of a chunk within a file's rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(transparent)]
pub struct ChunkIndex(u32);

impl ChunkIndex {
    pub const FIRST: ChunkIndex = ChunkIndex(0);

    #[must_use]
    pub const fn new(i: u32) -> Self {
        Self(i)
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// What a render is of: a diff between two blobs, or a single blob (explorer).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(RenderTargetKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum RenderTarget {
    Diff { change: ChangeKind },
    Blob { entry: BlobEntry },
}

impl RenderTarget {
    pub fn viewed_content(&self) -> crate::ViewedContent {
        match self {
            Self::Diff { change } => change.viewed_content(),
            Self::Blob { entry } => crate::ViewedContent::Blob { entry: *entry },
        }
    }
}

/// Row-level shape of a rendered file, known before any chunk.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, EnumDiscriminants)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[strum_discriminants(name(RenderContentKind), derive(EnumIter, Hash))]
#[serde(tag = "type", deny_unknown_fields)]
pub enum RenderContent {
    /// No source rows: the diff target describes typed gitlink identities.
    Submodule,
    /// No rows; UI shows a binary placeholder.
    Binary,
    Text {
        total_rows: u32,
        /// Rows per chunk (last chunk may be shorter).
        chunk_rows: u32,
        chunk_count: u32,
        /// `false` when the file exceeded the highlight size cap.
        highlighted: bool,
        additions: u32,
        deletions: u32,
        /// Where each still-hidden run's expander sits, in row order. The
        /// header is always cached for an open file, so `z u`/`z d` can
        /// name the gap beside the cursor without the chunk it lives in
        /// (a hunk can be taller than what the cache holds).
        #[serde(default)]
        gaps: GapTable,
    },
}

/// One gap's expander row: which gap, and the row it is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct GapRow {
    pub gap: crate::domain::Gap,
    pub row: u32,
}

/// Wire input that is not a canonical gap table.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GapTableError {
    #[error("gap rows are not in row order")]
    Unordered,
    #[error("gaps are not in gap order")]
    UnorderedGaps,
    #[error("gap {0:?} appears more than once")]
    Duplicate(crate::domain::Gap),
}

/// A render's gaps in row order, one entry per gap. Callers pick the
/// nearest gap above or below a row by walking this in order, so the
/// order is the invariant: it is parsed at the boundary rather than
/// trusted, or a malformed header would silently misdirect `z u`/`z d`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(try_from = "Vec<GapRow>", into = "Vec<GapRow>")]
pub struct GapTable(Vec<GapRow>);

impl TryFrom<Vec<GapRow>> for GapTable {
    type Error = GapTableError;

    fn try_from(v: Vec<GapRow>) -> Result<Self, Self::Error> {
        // Both keys ascend strictly, which is what the renderer emits: a
        // gap precedes the group it is numbered for, so a later gap is
        // always a later row. Checking adjacent pairs is then enough for
        // uniqueness — comparing neighbours in a sequence that only ever
        // increases rules out a repeat anywhere, adjacent or not.
        for w in v.windows(2) {
            if w[0].gap == w[1].gap {
                return Err(GapTableError::Duplicate(w[0].gap));
            }
            if w[0].gap > w[1].gap {
                return Err(GapTableError::UnorderedGaps);
            }
            if w[0].row >= w[1].row {
                return Err(GapTableError::Unordered);
            }
        }
        Ok(Self(v))
    }
}

impl From<GapTable> for Vec<GapRow> {
    fn from(t: GapTable) -> Self {
        t.0
    }
}

impl GapTable {
    #[must_use]
    pub fn as_slice(&self) -> &[GapRow] {
        &self.0
    }

    /// The gap whose expander is nearest at or above (below) `row`.
    #[must_use]
    pub fn nearest(&self, row: u32, up: bool) -> Option<crate::domain::Gap> {
        if up {
            self.0.iter().rev().find(|g| g.row <= row).map(|g| g.gap)
        } else {
            self.0.iter().find(|g| g.row >= row).map(|g| g.gap)
        }
    }
}

/// Header for a rendered file: everything but the rows.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FileRenderHeader {
    pub repo_id: RepoId,
    pub path: RepoPath,
    pub target: RenderTarget,
    pub opts: RenderOpts,
    /// Language used for highlighting, as a syntect/linguist-style name.
    pub lang: Option<String>,
    pub content: RenderContent,
}

/// A contiguous slice of a file's rows.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct RenderChunk {
    pub index: ChunkIndex,
    pub rows: Vec<Row>,
}

/// A whole rendered file, for non-streamed consumers (CLI, MCP).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FileRender {
    pub header: FileRenderHeader,
    pub chunks: Vec<RenderChunk>,
}

/// Per-file line counts, for the tree and progress views.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct FileSummary {
    pub repo_id: RepoId,
    pub path: RepoPath,
    pub change: ChangeKind,
    pub additions: u32,
    pub deletions: u32,
    pub binary: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct DiffSummary {
    pub files: Vec<FileSummary>,
    pub additions: u32,
    pub deletions: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Gap;

    fn at(gap: u32, row: u32) -> GapRow {
        GapRow {
            gap: Gap::new(gap),
            row,
        }
    }

    #[test]
    fn a_gap_table_is_parsed_into_the_order_its_readers_walk() {
        // `nearest` walks the table in order; a header that arrived out of
        // order would silently send `z u` to the wrong gap.
        let t = GapTable::try_from(vec![at(0, 4), at(1, 40), at(2, 90)]).unwrap();
        assert_eq!(t.nearest(50, true), Some(Gap::new(1)), "the run above");
        assert_eq!(t.nearest(50, false), Some(Gap::new(2)), "the run below");
        assert_eq!(t.nearest(40, true), Some(Gap::new(1)), "on the expander");
        assert_eq!(t.nearest(95, false), None, "nothing hidden below");
        assert_eq!(t.nearest(2, true), None, "nothing hidden above");

        // Out of order by either key is refused; the gap column is
        // checked first, since it is the one `nearest` names.
        assert_eq!(
            GapTable::try_from(vec![at(1, 40), at(0, 4)]),
            Err(GapTableError::UnorderedGaps)
        );
        assert_eq!(
            GapTable::try_from(vec![at(0, 40), at(1, 4)]),
            Err(GapTableError::Unordered)
        );
        assert_eq!(
            GapTable::try_from(vec![at(1, 40), at(1, 90)]),
            Err(GapTableError::Duplicate(Gap::new(1)))
        );
        // Two expanders cannot share a row either.
        assert_eq!(
            GapTable::try_from(vec![at(0, 40), at(1, 40)]),
            Err(GapTableError::Unordered)
        );
        // A repeat that is not adjacent is a repeat all the same: the
        // gap column ascends strictly, so it cannot come back.
        assert_eq!(
            GapTable::try_from(vec![at(1, 4), at(2, 40), at(1, 90)]),
            Err(GapTableError::UnorderedGaps)
        );
        assert_eq!(
            GapTable::try_from(vec![at(0, 4), at(2, 40), at(1, 90), at(2, 120)]),
            Err(GapTableError::UnorderedGaps)
        );
        assert!(
            serde_json::from_str::<GapTable>(r#"[{"gap":1,"row":40},{"gap":0,"row":4}]"#).is_err()
        );
        let t = GapTable::try_from(vec![at(0, 4)]).unwrap();
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(serde_json::from_str::<GapTable>(&json).unwrap(), t);
    }
}
