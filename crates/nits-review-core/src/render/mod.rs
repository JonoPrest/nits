//! Diff render model: turn two blobs into the flat row list the UI shows.
//!
//! Pipeline (`docs/ARCHITECTURE.md` §4.6, `PLAN.md` 1.5):
//!
//! 1. split both sides into lines (CRLF and missing trailing newline are
//!    tolerated; rows carry the text without its terminator);
//! 2. diff with `imara-diff` — over a whitespace-stripped view of each line
//!    when `opts.ignore_whitespace` is set, while rows keep the real text;
//! 3. pair `-`/`+` runs positionally into `Modified` rows with word-level
//!    intra-line `changed` ranges;
//! 4. collapse unchanged runs longer than `2 * context_lines` into
//!    `Expander` rows and prefix each visible hunk with a `HunkHeader`;
//! 5. syntax-highlight each side in one whole-file pass (skipped above a
//!    size cap; `highlighted == false` then) and attach spans to cells;
//! 6. cut the rows into fixed-size chunks.
//!
//! Everything here is a pure function of `(old, new, lang, opts)`; the
//! [`cache`] module keys on OIDs so results are shared across clients.

pub mod cache;
mod highlight;
mod intraline;
mod lines;

use nits_protocol::{
    Cell, ChunkIndex, ColRange, ExpandDir, Expansions, Gap, LineNo, RenderChunk, RenderContent,
    RenderOpts, Row,
};

pub use highlight::{Highlighter, detect_lang};
use lines::{Line, split_lines};

/// Split a blob into line texts (no terminators), as the render model sees
/// them. Used by anchoring so line numbers agree with rendered rows.
#[must_use]
pub fn lines_of(bytes: &[u8]) -> Vec<String> {
    split_lines(bytes).into_iter().map(|l| l.text).collect()
}

/// Rows per chunk. Fixed so chunk indexes are stable for a given render.
pub const CHUNK_ROWS: u32 = 500;

/// Above this many bytes on either side, highlighting is skipped.
pub const HIGHLIGHT_BYTE_CAP: usize = 1 << 20;
/// Above this many lines on either side, highlighting is skipped.
pub const HIGHLIGHT_LINE_CAP: usize = 20_000;

/// Output of [`render_file`] / [`render_blob`]: the header's content
/// description plus every row. Pure data; chunk on demand.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rendered {
    pub content: RenderContent,
    pub rows: Vec<Row>,
}

impl Rendered {
    #[must_use]
    pub fn chunk_count(&self) -> u32 {
        chunk_count(self.rows.len())
    }

    /// The `i`th chunk, or `None` past the end.
    #[must_use]
    pub fn chunk(&self, index: ChunkIndex) -> Option<RenderChunk> {
        let start = (index.get() as usize).checked_mul(CHUNK_ROWS as usize)?;
        if start >= self.rows.len() && !(self.rows.is_empty() && index.get() == 0) {
            return None;
        }
        let end = (start + CHUNK_ROWS as usize).min(self.rows.len());
        Some(RenderChunk {
            index,
            rows: self.rows[start..end].to_vec(),
        })
    }

    pub fn chunks(&self) -> impl Iterator<Item = RenderChunk> + '_ {
        (0..self.chunk_count()).map(move |i| RenderChunk {
            index: ChunkIndex::new(i),
            rows: self.rows[i as usize * CHUNK_ROWS as usize
                ..((i as usize + 1) * CHUNK_ROWS as usize).min(self.rows.len())]
                .to_vec(),
        })
    }
}

fn chunk_count(rows: usize) -> u32 {
    let n = rows.div_ceil(CHUNK_ROWS as usize);
    u32::try_from(n.max(1)).unwrap_or(u32::MAX)
}

/// Render the diff of `old` → `new`. `None` on a side means the file does not
/// exist there (added / deleted). `lang` is a syntect syntax name from
/// [`detect_lang`]; `None` renders without spans.
#[must_use]
pub fn render_file(
    hl: &Highlighter,
    old: Option<&[u8]>,
    new: Option<&[u8]>,
    lang: Option<&str>,
    opts: &RenderOpts,
) -> Rendered {
    if old.is_some_and(crate::git::is_binary) || new.is_some_and(crate::git::is_binary) {
        return Rendered {
            content: RenderContent::Binary,
            rows: vec![],
        };
    }
    let old_lines = split_lines(old.unwrap_or_default());
    let new_lines = split_lines(new.unwrap_or_default());

    let hunks = diff_hunks(&old_lines, &new_lines, opts.ignore_whitespace);
    if hunks.is_empty() && opts.ignore_whitespace && old != new {
        return Rendered {
            content: RenderContent::Text {
                total_rows: 1,
                chunk_rows: CHUNK_ROWS,
                chunk_count: 1,
                highlighted: false,
                additions: 0,
                deletions: 0,
                gaps: nits_protocol::GapTable::default(),
            },
            rows: vec![Row::WhitespaceOnly],
        };
    }

    let highlighted = should_highlight(old, new, &old_lines, &new_lines, lang);
    let old_spans = spans_for(hl, highlighted, lang, &old_lines);
    let new_spans = spans_for(hl, highlighted, lang, &new_lines);

    let (rows, additions, deletions) = build_rows(
        &old_lines,
        &new_lines,
        &old_spans,
        &new_spans,
        &hunks,
        opts.context_lines,
        &opts.expanded,
    );
    let total_rows = u32::try_from(rows.len()).unwrap_or(u32::MAX);
    Rendered {
        content: RenderContent::Text {
            total_rows,
            chunk_rows: CHUNK_ROWS,
            chunk_count: chunk_count(rows.len()),
            highlighted,
            additions,
            deletions,
            gaps: gaps_of(&rows),
        },
        rows,
    }
}

/// Render a single blob for the explorer: every line as a `Context` row.
#[must_use]
pub fn render_blob(hl: &Highlighter, bytes: &[u8], lang: Option<&str>) -> Rendered {
    if crate::git::is_binary(bytes) {
        return Rendered {
            content: RenderContent::Binary,
            rows: vec![],
        };
    }
    let lines = split_lines(bytes);
    let highlighted = should_highlight(Some(bytes), Some(bytes), &lines, &lines, lang);
    let spans = spans_for(hl, highlighted, lang, &lines);
    let rows: Vec<Row> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let cell = cell(i, l, &spans, vec![]);
            Row::Context {
                left: cell.clone(),
                right: cell,
            }
        })
        .collect();
    let total_rows = u32::try_from(rows.len()).unwrap_or(u32::MAX);
    Rendered {
        content: RenderContent::Text {
            total_rows,
            chunk_rows: CHUNK_ROWS,
            chunk_count: chunk_count(rows.len()),
            highlighted,
            additions: 0,
            deletions: 0,
            gaps: nits_protocol::GapTable::default(),
        },
        rows,
    }
}

fn should_highlight(
    old: Option<&[u8]>,
    new: Option<&[u8]>,
    old_lines: &[Line],
    new_lines: &[Line],
    lang: Option<&str>,
) -> bool {
    lang.is_some()
        && old.is_none_or(|b| b.len() <= HIGHLIGHT_BYTE_CAP)
        && new.is_none_or(|b| b.len() <= HIGHLIGHT_BYTE_CAP)
        && old_lines.len() <= HIGHLIGHT_LINE_CAP
        && new_lines.len() <= HIGHLIGHT_LINE_CAP
}

fn spans_for(
    hl: &Highlighter,
    enabled: bool,
    lang: Option<&str>,
    lines: &[Line],
) -> Vec<Vec<nits_protocol::Span>> {
    match (enabled, lang) {
        (true, Some(lang)) => hl.highlight(lang, lines.iter().map(|l| l.text.as_str())),
        _ => vec![Vec::new(); lines.len()],
    }
}

/// A hunk in line indexes: `before` on the old side, `after` on the new.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Hunk {
    before: std::ops::Range<u32>,
    after: std::ops::Range<u32>,
}

fn diff_hunks(old: &[Line], new: &[Line], ignore_whitespace: bool) -> Vec<Hunk> {
    fn key(l: &Line, ignore_whitespace: bool) -> LineKey<'_> {
        if ignore_whitespace {
            LineKey {
                text: &l.normalised,
                ending: nits_protocol::LineEnding::Missing,
            }
        } else {
            LineKey {
                text: &l.text,
                ending: l.ending,
            }
        }
    }
    let before: Vec<_> = old.iter().map(|l| key(l, ignore_whitespace)).collect();
    let after: Vec<_> = new.iter().map(|l| key(l, ignore_whitespace)).collect();
    let input = imara_diff::InternedInput::new(LineKeys(&before), LineKeys(&after));
    let mut diff = imara_diff::Diff::compute(imara_diff::Algorithm::Histogram, &input);
    diff.postprocess_lines(&input);
    diff.hunks()
        .map(|h| Hunk {
            before: h.before,
            after: h.after,
        })
        .collect()
}

/// Equality includes the terminator; indentation postprocessing reads source text.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct LineKey<'a> {
    text: &'a str,
    ending: nits_protocol::LineEnding,
}

impl AsRef<[u8]> for LineKey<'_> {
    fn as_ref(&self) -> &[u8] {
        self.text.as_bytes()
    }
}

/// `TokenSource` over a slice of already-split lines.
struct LineKeys<'a>(&'a [LineKey<'a>]);

impl<'a> imara_diff::TokenSource for LineKeys<'a> {
    type Token = LineKey<'a>;
    type Tokenizer = std::iter::Copied<std::slice::Iter<'a, Self::Token>>;
    fn tokenize(&self) -> Self::Tokenizer {
        self.0.iter().copied()
    }
    fn estimate_tokens(&self) -> u32 {
        u32::try_from(self.0.len()).unwrap_or(u32::MAX)
    }
}

fn cell(
    index: usize,
    line: &Line,
    spans: &[Vec<nits_protocol::Span>],
    changed: Vec<ColRange>,
) -> Cell {
    Cell {
        line_no: LineNo::from_index(u32::try_from(index).unwrap_or(u32::MAX - 1)),
        text: line.text.clone(),
        ending: line.ending,
        spans: spans.get(index).cloned().unwrap_or_default(),
        changed,
    }
}

/// A group's visible window, in old and new line coordinates.
struct Window {
    start_o: usize,
    end_o: usize,
    start_n: usize,
    end_n: usize,
}

/// Where each still-hidden run's expander sits. Carried on the header so
/// a client can name the gap beside the cursor without holding the chunk
/// the expander lives in.
fn gaps_of(rows: &[Row]) -> nits_protocol::GapTable {
    let table: Vec<nits_protocol::GapRow> = rows
        .iter()
        .enumerate()
        .filter_map(|(i, r)| match r {
            Row::Expander { gap, .. } => Some(nits_protocol::GapRow {
                gap: *gap,
                row: u32::try_from(i).unwrap_or(u32::MAX),
            }),
            Row::HunkHeader { .. }
            | Row::Context { .. }
            | Row::Added { .. }
            | Row::Removed { .. }
            | Row::Modified { .. }
            | Row::WhitespaceOnly => None,
        })
        .collect();
    // Built in row order, one entry per gap, by construction.
    nits_protocol::GapTable::try_from(table).unwrap_or_default()
}

/// Build the row list with context collapsing. Returns
/// `(rows, additions, deletions)`.
///
/// Hunks whose separating equal run is `<= 2 * context` are merged into one
/// display group with a single header, exactly as `git diff` does.
#[allow(clippy::too_many_lines)] // one linear pass; splitting it would obscure the cursor logic
fn build_rows(
    old: &[Line],
    new: &[Line],
    old_spans: &[Vec<nits_protocol::Span>],
    new_spans: &[Vec<nits_protocol::Span>],
    hunks: &[Hunk],
    context: u32,
    expanded: &Expansions,
) -> (Vec<Row>, u32, u32) {
    let ctx = context as usize;
    let mut rows = Vec::new();
    let (mut additions, mut deletions) = (0u32, 0u32);
    let to_u32 = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);

    if hunks.is_empty() {
        // One gap, the whole file; opening it reveals lines from either
        // end, since there is no hunk to expand away from.
        if !old.is_empty() {
            let (up, down) = expanded.of(Gap::new(0));
            let shown = (up as usize).saturating_add(down as usize).min(old.len());
            let head = (down as usize).min(shown);
            let tail = shown - head;
            emit_context(&mut rows, old, new, old_spans, new_spans, 0, 0, head);
            let hidden = old.len() - shown;
            if hidden > 0 {
                rows.push(Row::Expander {
                    hidden: to_u32(hidden),
                    dir: ExpandDir::Both,
                    gap: Gap::new(0),
                });
            }
            let from = old.len() - tail;
            emit_context(&mut rows, old, new, old_spans, new_spans, from, from, tail);
        }
        return (rows, 0, 0);
    }

    // Group hunks separated by at most 2*ctx equal lines.
    let mut groups: Vec<std::ops::Range<usize>> = Vec::new();
    groups.push(0..1);
    for (i, h) in hunks.iter().enumerate().skip(1) {
        let prev = &hunks[i - 1];
        let gap = h.before.start as usize - prev.before.end as usize;
        if gap <= 2 * ctx {
            groups.last_mut().expect("non-empty").end = i + 1;
        } else {
            groups.push(i..i + 1);
        }
    }

    // Each group's visible window, in old and new coordinates: `ctx`
    // lines around its changes, plus however far its own gaps have been
    // opened — the gap above it upward, the gap below it downward. A
    // trailing expansion stops at the next group's first change; past
    // that the lines belong to the next window.
    let (mut oi, mut ni) = (0usize, 0usize);
    let mut windows: Vec<Window> = Vec::with_capacity(groups.len());
    for (g_idx, g) in groups.iter().enumerate() {
        let first = &hunks[g.start];
        let last = &hunks[g.end - 1];
        let (up, _) = expanded.of(Gap::new(to_u32(g_idx)));
        let (_, down) = expanded.of(Gap::new(to_u32(g_idx + 1)));
        let lead = ctx.saturating_add(up as usize);
        let start_o = (first.before.start as usize).saturating_sub(lead).max(oi);
        let start_n = ni + (start_o - oi);
        let trail = ctx.saturating_add(down as usize);
        let next_change = groups
            .get(g_idx + 1)
            .map_or(old.len(), |ng| hunks[ng.start].before.start as usize);
        let end_o = (last.before.end as usize + trail)
            .min(old.len())
            .min(next_change);
        let end_n = last.after.end as usize + (end_o - last.before.end as usize);
        windows.push(Window {
            start_o,
            end_o,
            start_n,
            end_n,
        });
        oi = end_o;
        ni = end_n;
    }

    // Groups whose windows meet — because the gap between them was opened
    // all the way — are one block under one header, as git prints
    // overlapping hunks.
    let mut blocks: Vec<std::ops::Range<usize>> = Vec::new();
    for g_idx in 0..groups.len() {
        match blocks.last_mut() {
            Some(b) if windows[g_idx].start_o <= windows[b.end - 1].end_o => b.end = g_idx + 1,
            Some(_) | None => blocks.push(g_idx..g_idx + 1),
        }
    }

    let (mut oi, mut ni) = (0usize, 0usize);
    for b in &blocks {
        let head = &windows[b.start];
        let tail = &windows[b.end - 1];
        let hidden_before = head.start_o - oi;
        if hidden_before > 0 {
            rows.push(Row::Expander {
                hidden: to_u32(hidden_before),
                dir: if b.start == 0 {
                    ExpandDir::Up
                } else {
                    ExpandDir::Both
                },
                gap: Gap::new(to_u32(b.start)),
            });
        }
        // One header over the whole block, not over its first group.
        rows.push(Row::HunkHeader {
            text: hunk_header(
                head.start_o,
                tail.end_o - head.start_o,
                head.start_n,
                tail.end_n - head.start_n,
            ),
        });
        let (mut co, mut cn) = (head.start_o, head.start_n);
        for g in &groups[b.clone()] {
            for h in &hunks[g.clone()] {
                let eq = h.before.start as usize - co;
                emit_context(&mut rows, old, new, old_spans, new_spans, co, cn, eq);
                let removed = h.before.start as usize..h.before.end as usize;
                let added = h.after.start as usize..h.after.end as usize;
                let paired = removed.len().min(added.len());
                for k in 0..paired {
                    let (o, n) = (removed.start + k, added.start + k);
                    let (lc, rc) = intraline::changed_ranges(&old[o].text, &new[n].text);
                    rows.push(Row::Modified {
                        left: cell(o, &old[o], old_spans, lc),
                        right: cell(n, &new[n], new_spans, rc),
                    });
                }
                rows.extend((removed.start + paired..removed.end).map(|o| Row::Removed {
                    left: cell(o, &old[o], old_spans, vec![]),
                }));
                rows.extend((added.start + paired..added.end).map(|n| Row::Added {
                    right: cell(n, &new[n], new_spans, vec![]),
                }));
                deletions += to_u32(removed.len());
                additions += to_u32(added.len());
                co = h.before.end as usize;
                cn = h.after.end as usize;
            }
        }
        emit_context(
            &mut rows,
            old,
            new,
            old_spans,
            new_spans,
            co,
            cn,
            tail.end_o - co,
        );
        oi = tail.end_o;
        ni = tail.end_n;
    }
    let _ = ni;

    let hidden_tail = old.len() - oi;
    if hidden_tail > 0 {
        rows.push(Row::Expander {
            hidden: to_u32(hidden_tail),
            dir: ExpandDir::Down,
            gap: Gap::new(to_u32(groups.len())),
        });
    }
    (rows, additions, deletions)
}

#[allow(clippy::too_many_arguments)]
fn emit_context(
    rows: &mut Vec<Row>,
    old: &[Line],
    new: &[Line],
    old_spans: &[Vec<nits_protocol::Span>],
    new_spans: &[Vec<nits_protocol::Span>],
    oi: usize,
    ni: usize,
    count: usize,
) {
    for k in 0..count {
        rows.push(Row::Context {
            left: cell(oi + k, &old[oi + k], old_spans, vec![]),
            right: cell(ni + k, &new[ni + k], new_spans, vec![]),
        });
    }
}

fn hunk_header(old_start: usize, old_len: usize, new_start: usize, new_len: usize) -> String {
    // git prints the 1-based start, or 0 when the range is empty.
    let s = |start: usize, len: usize| if len == 0 { start } else { start + 1 };
    format!(
        "@@ -{},{} +{},{} @@",
        s(old_start, old_len),
        old_len,
        s(new_start, new_len),
        new_len
    )
}
