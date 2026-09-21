#![allow(clippy::format_collect, clippy::naive_bytecount)] // test data builders
//! Render model: snapshot corpus + invariants.

use nits_protocol::{ChunkIndex, ExpandDir, Expansions, Gap, RenderContent, RenderOpts, Row};
use nits_review_core::render::{CHUNK_ROWS, Highlighter, Rendered, render_blob, render_file};
use proptest::prelude::*;
use std::sync::LazyLock;

static HL: LazyLock<Highlighter> = LazyLock::new(Highlighter::new);

fn opts(ignore_ws: bool) -> RenderOpts {
    RenderOpts {
        ignore_whitespace: ignore_ws,
        context_lines: 3,
        ..RenderOpts::default()
    }
}

#[test]
fn exact_terminator_changes_keep_source_coordinates_and_ignore_policy() {
    use nits_protocol::LineEnding;
    for old in [LineEnding::Lf, LineEnding::CrLf, LineEnding::Missing] {
        for new in [LineEnding::Lf, LineEnding::CrLf, LineEnding::Missing] {
            if old == new {
                continue;
            }
            let before = format!("same{}", old.as_str());
            let after = format!("same{}", new.as_str());
            let rendered = render_file(
                &HL,
                Some(before.as_bytes()),
                Some(after.as_bytes()),
                None,
                &opts(false),
            );
            assert!(matches!(
                rendered.content,
                RenderContent::Text {
                    additions: 1,
                    deletions: 1,
                    ..
                }
            ));
            let cells: Vec<_> = rendered
                .rows
                .iter()
                .filter_map(|row| match row {
                    Row::Modified { left, right } => Some((left, right)),
                    _ => None,
                })
                .collect();
            assert_eq!(cells.len(), 1);
            let (left, right) = cells[0];
            assert_eq!((left.line_no.get(), right.line_no.get()), (1, 1));
            assert_eq!((left.text.as_str(), right.text.as_str()), ("same", "same"));
            assert_eq!((left.ending, right.ending), (old, new));
            assert!(left.changed.is_empty() && right.changed.is_empty());
            let ignored = render_file(
                &HL,
                Some(before.as_bytes()),
                Some(after.as_bytes()),
                None,
                &opts(true),
            );
            assert_eq!(ignored.rows, vec![Row::WhitespaceOnly]);
        }
    }
    let bare_cr = render_file(&HL, Some(b"same"), Some(b"same\r"), None, &opts(false));
    assert!(bare_cr.rows.iter().any(|r| matches!(r,Row::Modified {right,..} if right.text == "same\r" && right.ending == LineEnding::Missing)));
}

#[test]
fn endings_survive_expansion_browse_and_chunk_boundaries_without_extra_source_lines() {
    use nits_protocol::LineEnding;
    let before = format!("{}last", "context\r\n".repeat(502));
    let after = format!("{before}\n");
    let opts = RenderOpts {
        context_lines: u32::MAX,
        ..opts(false)
    };
    let rendered = render_file(
        &HL,
        Some(before.as_bytes()),
        Some(after.as_bytes()),
        None,
        &opts,
    );
    let chunks: Vec<_> = rendered.chunks().collect();
    assert!(chunks.len() > 1);
    let mut source_lines = Vec::new();
    for row in chunks.iter().flat_map(|c| &c.rows) {
        match row {
            Row::Context { left, right } => {
                assert_eq!(left.ending, LineEnding::CrLf);
                assert_eq!(right.ending, LineEnding::CrLf);
                source_lines.push(right.line_no.get());
            }
            Row::Modified { left, right } => {
                assert_eq!(
                    (left.ending, right.ending),
                    (LineEnding::Missing, LineEnding::Lf)
                );
                source_lines.push(right.line_no.get());
            }
            Row::HunkHeader { .. } => {}
            _ => panic!("unexpected source row {row:?}"),
        }
    }
    assert_eq!(source_lines, (1..=503).collect::<Vec<_>>());
    for bytes in [
        b"".as_slice(),
        b"\n",
        b"\r\n",
        b"x\r",
        before.as_bytes(),
        after.as_bytes(),
    ] {
        let browse = render_blob(&HL, bytes, None);
        let rebuilt: String = browse
            .rows
            .iter()
            .map(|row| match row {
                Row::Context { right, .. } => format!("{}{}", right.text, right.ending.as_str()),
                _ => panic!("blob has only source rows"),
            })
            .collect();
        assert_eq!(rebuilt.as_bytes(), bytes);
    }
}

const RUST_OLD: &str = "\
use std::fmt;

fn main() {
    let x = 1;
    let y = 2;
    println!(\"{}\", x + y);
}

fn helper(a: u32) -> u32 {
    a * 2
}

fn unchanged_1() {}
fn unchanged_2() {}
fn unchanged_3() {}
fn unchanged_4() {}
fn unchanged_5() {}
fn unchanged_6() {}
fn unchanged_7() {}
fn unchanged_8() {}

fn tail() -> bool {
    true
}
";

const RUST_NEW: &str = "\
use std::fmt;

fn main() {
    let x: u32 = 1;
    let y = 2;
    let z = 3;
    println!(\"{}\", x + y + z);
}

fn helper(a: u32) -> u32 {
    a * 2
}

fn unchanged_1() {}
fn unchanged_2() {}
fn unchanged_3() {}
fn unchanged_4() {}
fn unchanged_5() {}
fn unchanged_6() {}
fn unchanged_7() {}
fn unchanged_8() {}

fn tail() -> bool {
    false
}
";

struct Case {
    name: &'static str,
    old: Option<&'static [u8]>,
    new: Option<&'static [u8]>,
    lang: Option<&'static str>,
}

fn corpus() -> Vec<Case> {
    vec![
        Case {
            name: "added",
            old: None,
            new: Some(RUST_NEW.as_bytes()),
            lang: Some("Rust"),
        },
        Case {
            name: "deleted",
            old: Some(RUST_OLD.as_bytes()),
            new: None,
            lang: Some("Rust"),
        },
        Case {
            name: "modified",
            old: Some(RUST_OLD.as_bytes()),
            new: Some(RUST_NEW.as_bytes()),
            lang: Some("Rust"),
        },
        Case {
            name: "unhighlighted",
            old: Some(RUST_OLD.as_bytes()),
            new: Some(RUST_NEW.as_bytes()),
            lang: None,
        },
        Case {
            name: "whitespace_only",
            old: Some(b"fn a() {\n    x();\n}\n"),
            new: Some(b"fn a() {\n\tx();\n}\n"),
            lang: Some("Rust"),
        },
        Case {
            name: "reindented_block_plus_real_change",
            old: Some(b"a\nb\n  c\n  d\ne\nf\n"),
            new: Some(b"a\nb\n    c\n    d\ne\nF\n"),
            lang: None,
        },
        Case {
            name: "binary",
            old: Some(b"\x00\x01\x02"),
            new: Some(b"\x00\x01\x03"),
            lang: None,
        },
        Case {
            name: "no_trailing_newline",
            old: Some(b"one\ntwo"),
            new: Some(b"one\ntwo\n"),
            lang: None,
        },
        Case {
            name: "crlf",
            old: Some(b"a\r\nb\r\nc\r\n"),
            new: Some(b"a\r\nB\r\nc\r\n"),
            lang: None,
        },
        Case {
            name: "removed_final_newline",
            old: Some(b"same\n"),
            new: Some(b"same"),
            lang: None,
        },
        Case {
            name: "lf_to_crlf",
            old: Some(b"one\ntwo\n"),
            new: Some(b"one\r\ntwo\r\n"),
            lang: None,
        },
        Case {
            name: "mixed_terminators",
            old: Some(b"one\ntwo\nthree\n"),
            new: Some(b"one\r\nchanged\nthree"),
            lang: None,
        },
        Case {
            name: "bare_cr_content",
            old: Some(b"same"),
            new: Some(b"same\r"),
            lang: None,
        },
        Case {
            name: "identical",
            old: Some(b"a\nb\nc\n"),
            new: Some(b"a\nb\nc\n"),
            lang: None,
        },
        Case {
            name: "empty_to_empty",
            old: Some(b""),
            new: Some(b""),
            lang: None,
        },
    ]
}

#[test]
fn snapshot_corpus() {
    for case in corpus() {
        for ignore_ws in [false, true] {
            let r = render_file(&HL, case.old, case.new, case.lang, &opts(ignore_ws));
            check_invariants(&r, case.old, case.new);
            let suffix = if ignore_ws { "ignore_ws" } else { "exact" };
            insta::assert_json_snapshot!(format!("{}__{suffix}", case.name), r);
        }
    }
}

#[test]
fn blob_render_is_all_context() {
    let r = render_blob(&HL, RUST_NEW.as_bytes(), Some("Rust"));
    assert!(r.rows.iter().all(|row| matches!(row, Row::Context { .. })));
    assert_eq!(r.rows.len(), RUST_NEW.lines().count());
    insta::assert_json_snapshot!("blob_rust", r);
}

#[test]
fn ignore_whitespace_hides_reindent_but_keeps_text() {
    let old = b"fn a() {\n    x();\n    y();\n}\n";
    let new = b"fn a() {\n        x();\n        y();\n}\n";
    let exact = render_file(&HL, Some(old), Some(new), None, &opts(false));
    assert_eq!(
        exact
            .rows
            .iter()
            .filter(|r| matches!(r, Row::Modified { .. }))
            .count(),
        2
    );
    let ws = render_file(&HL, Some(old), Some(new), None, &opts(true));
    assert_eq!(ws.rows, vec![Row::WhitespaceOnly]);

    // With a real change elsewhere, the reindented lines are plain context
    // and still carry their real (reindented) text.
    let new2 = b"fn a() {\n        x();\n        y();\n}\nextra\n";
    let ws2 = render_file(&HL, Some(old), Some(new2), None, &opts(true));
    assert_eq!(
        ws2.rows
            .iter()
            .filter(|r| matches!(r, Row::Modified { .. }))
            .count(),
        0
    );
    let ctx_texts: Vec<&str> = ws2
        .rows
        .iter()
        .filter_map(|r| match r {
            Row::Context { right, .. } => Some(right.text.as_str()),
            _ => None,
        })
        .collect();
    assert!(ctx_texts.contains(&"        x();"));
}

#[test]
fn chunks_are_fixed_size_and_concatenate() {
    let old: String = (0..1234).map(|i| format!("line {i}\n")).collect();
    let new: String = (0..1234)
        .map(|i| {
            if i % 100 == 0 {
                format!("LINE {i}\n")
            } else {
                format!("line {i}\n")
            }
        })
        .collect();
    let r = render_file(
        &HL,
        Some(old.as_bytes()),
        Some(new.as_bytes()),
        None,
        &RenderOpts {
            ignore_whitespace: false,
            context_lines: 1000,
            ..RenderOpts::default()
        },
    );
    let RenderContent::Text {
        total_rows,
        chunk_rows,
        chunk_count,
        ..
    } = r.content
    else {
        panic!()
    };
    assert_eq!(chunk_rows, CHUNK_ROWS);
    assert_eq!(total_rows as usize, r.rows.len());
    assert_eq!(chunk_count, r.chunk_count());
    let joined: Vec<Row> = r.chunks().flat_map(|c| c.rows).collect();
    assert_eq!(joined, r.rows);
    assert!(r.chunks().all(|c| c.rows.len() <= CHUNK_ROWS as usize));
    assert!(r.chunk(ChunkIndex::new(chunk_count)).is_none());
}

#[test]
fn large_file_renders_quickly_and_without_highlight_above_cap() {
    let old: String = (0..100_000)
        .map(|i| format!("fn f{i}() {{ let v = {i}; }}\n"))
        .collect();
    let mut new = old.clone();
    new.push_str("fn extra() {}\n");
    let start = std::time::Instant::now();
    let r = render_file(
        &HL,
        Some(old.as_bytes()),
        Some(new.as_bytes()),
        Some("Rust"),
        &opts(false),
    );
    let elapsed = start.elapsed();
    let RenderContent::Text {
        highlighted,
        additions,
        ..
    } = r.content
    else {
        panic!()
    };
    assert!(!highlighted, "100k lines is above the highlight cap");
    assert_eq!(additions, 1);
    assert!(elapsed.as_secs() < 5, "took {elapsed:?}");
}

// ---- invariants ------------------------------------------------------------

fn check_invariants(r: &Rendered, old: Option<&[u8]>, new: Option<&[u8]>) {
    let RenderContent::Text {
        total_rows,
        chunk_count,
        ..
    } = r.content
    else {
        assert!(r.rows.is_empty());
        return;
    };
    assert_eq!(total_rows as usize, r.rows.len());
    assert_eq!(chunk_count, r.chunk_count());
    if r.rows == vec![Row::WhitespaceOnly] {
        return;
    }
    let count_lines = |b: Option<&[u8]>| {
        b.map_or(0usize, |b| {
            let n = b.iter().filter(|c| **c == b'\n').count();
            if b.is_empty() || b.ends_with(b"\n") {
                n
            } else {
                n + 1
            }
        })
    };
    let (old_n, new_n) = (count_lines(old), count_lines(new));

    let (mut left_seen, mut right_seen, mut hidden) = (0usize, 0usize, 0usize);
    let (mut last_left, mut last_right) = (0u32, 0u32);
    for row in &r.rows {
        let cells: Vec<(&nits_protocol::Cell, bool)> = match row {
            Row::Context { left, right } | Row::Modified { left, right } => {
                vec![(left, true), (right, false)]
            }
            Row::Removed { left } => vec![(left, true)],
            Row::Added { right } => vec![(right, false)],
            Row::Expander { hidden: h, .. } => {
                hidden += *h as usize;
                vec![]
            }
            Row::HunkHeader { .. } | Row::WhitespaceOnly => vec![],
        };
        for (cell, is_left) in cells {
            let n = cell.line_no.get();
            if is_left {
                assert!(n > last_left, "left line numbers not increasing at {n}");
                last_left = n;
                left_seen += 1;
            } else {
                assert!(n > last_right, "right line numbers not increasing at {n}");
                last_right = n;
                right_seen += 1;
            }
            let len = u32::try_from(cell.text.len()).unwrap();
            let mut prev_end = 0;
            for s in &cell.spans {
                assert!(
                    s.range.start() >= prev_end && s.range.end() <= len,
                    "span {s:?} out of bounds/overlapping in {:?}",
                    cell.text
                );
                prev_end = s.range.end();
            }
            for c in &cell.changed {
                assert!(
                    c.end() <= len,
                    "changed {c:?} out of bounds in {:?}",
                    cell.text
                );
            }
        }
    }
    assert_eq!(
        left_seen + hidden,
        old_n,
        "every old line appears once (visible or hidden)"
    );
    assert_eq!(
        right_seen + hidden,
        new_n,
        "every new line appears once (visible or hidden)"
    );
    if let Some(Row::Expander { dir, .. }) = r.rows.first() {
        assert!(matches!(dir, ExpandDir::Up | ExpandDir::Both));
    }
    if let Some(Row::Expander { dir, .. }) = r.rows.last() {
        assert!(matches!(dir, ExpandDir::Down | ExpandDir::Both));
    }
}

fn text_strategy() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(
        prop_oneof![
            Just("a"),
            Just("b"),
            Just("c"),
            Just("  a"),
            Just("\tb"),
            Just(""),
            Just("x y")
        ],
        0..40,
    )
    .prop_map(|lines| {
        let mut s = lines.join("\n");
        if !s.is_empty() {
            s.push('\n');
        }
        s.into_bytes()
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(200))]

    #[test]
    fn invariants_hold(old in text_strategy(), new in text_strategy(), ctx in 0u32..5, ws in any::<bool>()) {
        let r = render_file(&HL, Some(&old), Some(&new), None, &RenderOpts { ignore_whitespace: ws, context_lines: ctx, ..RenderOpts::default() });
        check_invariants(&r, Some(&old), Some(&new));
    }

    #[test]
    fn edits_that_only_change_whitespace_never_produce_modified_rows(
        lines in prop::collection::vec("[a-z]{1,5}", 1..20),
        indents in prop::collection::vec(0usize..4, 1..20),
    ) {
        let old: String = lines.iter().map(|l| format!("{l}\n")).collect();
        let new: String = lines.iter().zip(indents.iter().cycle()).map(|(l, i)| format!("{}{l}\n", " ".repeat(*i))).collect();
        let r = render_file(&HL, Some(old.as_bytes()), Some(new.as_bytes()), None, &opts(true));
        prop_assert!(r.rows.iter().all(|row| !matches!(row, Row::Modified { .. } | Row::Added { .. } | Row::Removed { .. })), "{:?}", r.rows);
    }
}

// ---- per-gap expansion -----------------------------------------------------

/// A file with two changes far apart, so the render has three gaps:
/// before the first hunk (0), between the hunks (1), after the last (2).
fn two_hunks() -> (String, String) {
    let old: String = (1..=200).map(|i| format!("line {i}\n")).collect();
    let new = old
        .replace("line 20\n", "line 20 changed\n")
        .replace("line 180\n", "line 180 changed\n");
    (old, new)
}

fn expanded(gap: u32, up: u32, down: u32) -> RenderOpts {
    let gap = Gap::new(gap);
    RenderOpts {
        expanded: Expansions::default().opened(
            gap,
            match (up, down) {
                (0, _) => ExpandDir::Down,
                (_, 0) => ExpandDir::Up,
                _ => ExpandDir::Both,
            },
            up.max(down),
        ),
        ..opts(false)
    }
}

/// The lines each side shows, and the hidden run of each gap.
fn shown(r: &Rendered) -> (Vec<u32>, Vec<(Gap, u32)>) {
    let mut lines = Vec::new();
    let mut gaps = Vec::new();
    for row in &r.rows {
        match row {
            Row::Context { left, .. } | Row::Modified { left, .. } | Row::Removed { left } => {
                lines.push(left.line_no.get());
            }
            Row::Expander { hidden, gap, .. } => gaps.push((*gap, *hidden)),
            Row::Added { .. } | Row::HunkHeader { .. } | Row::WhitespaceOnly => {}
        }
    }
    (lines, gaps)
}

#[test]
fn every_gap_is_numbered_and_expansion_moves_only_its_own_gap() {
    let (old, new) = two_hunks();
    let (old, new) = (old.as_bytes(), new.as_bytes());
    let base = render_file(&HL, Some(old), Some(new), None, &opts(false));
    let (base_lines, base_gaps) = shown(&base);
    assert_eq!(
        base_gaps.iter().map(|(g, _)| g.get()).collect::<Vec<_>>(),
        vec![0, 1, 2],
        "one gap before each hunk group and one after the last"
    );
    check_invariants(&base, Some(old), Some(new));

    // Open the middle gap upward: the lines revealed sit above the second
    // hunk, gap 1 shrinks by exactly that many, and nothing else moves.
    let up = render_file(&HL, Some(old), Some(new), None, &expanded(1, 10, 0));
    let (up_lines, up_gaps) = shown(&up);
    check_invariants(&up, Some(old), Some(new));
    assert_eq!(gap_hidden(&base_gaps, 1) - gap_hidden(&up_gaps, 1), 10);
    assert_eq!(gap_hidden(&base_gaps, 0), gap_hidden(&up_gaps, 0));
    assert_eq!(gap_hidden(&base_gaps, 2), gap_hidden(&up_gaps, 2));
    let revealed: Vec<u32> = up_lines
        .iter()
        .copied()
        .filter(|l| !base_lines.contains(l))
        .collect();
    assert_eq!(revealed.len(), 10);
    assert!(
        revealed.iter().all(|l| *l < 177 && *l > 166),
        "revealed above the second hunk: {revealed:?}"
    );

    // Downward opens the same gap from the other end.
    let down = render_file(&HL, Some(old), Some(new), None, &expanded(1, 0, 10));
    let (down_lines, down_gaps) = shown(&down);
    check_invariants(&down, Some(old), Some(new));
    assert_eq!(gap_hidden(&base_gaps, 1) - gap_hidden(&down_gaps, 1), 10);
    let revealed: Vec<u32> = down_lines
        .iter()
        .copied()
        .filter(|l| !base_lines.contains(l))
        .collect();
    assert!(
        revealed.iter().all(|l| *l > 23 && *l < 34),
        "revealed below the first hunk: {revealed:?}"
    );

    // Widening the file's context, by contrast, moves every hunk.
    let wide = render_file(
        &HL,
        Some(old),
        Some(new),
        None,
        &RenderOpts {
            context_lines: 13,
            ..opts(false)
        },
    );
    let (_, wide_gaps) = shown(&wide);
    assert!(gap_hidden(&base_gaps, 0) > gap_hidden(&wide_gaps, 0));
    assert!(gap_hidden(&base_gaps, 2) > gap_hidden(&wide_gaps, 2));
}

fn gap_hidden(gaps: &[(Gap, u32)], gap: u32) -> u32 {
    let gap = Gap::new(gap);
    gaps.iter().find(|(g, _)| *g == gap).map_or(0, |(_, h)| *h)
}

#[test]
fn a_gap_opened_past_its_end_closes_and_leaves_one_header() {
    let (old, new) = two_hunks();
    let (old, new) = (old.as_bytes(), new.as_bytes());
    let all = render_file(&HL, Some(old), Some(new), None, &expanded(1, 500, 500));
    let (_, gaps) = shown(&all);
    check_invariants(&all, Some(old), Some(new));
    assert_eq!(gap_hidden(&gaps, 1), 0, "the middle run is fully revealed");
    let headers: Vec<String> = all
        .rows
        .iter()
        .filter_map(|r| match r {
            Row::HunkHeader { text } => Some(text.clone()),
            Row::Context { .. }
            | Row::Removed { .. }
            | Row::Added { .. }
            | Row::Modified { .. }
            | Row::Expander { .. }
            | Row::WhitespaceOnly => None,
        })
        .collect();
    // One header, and it covers the whole joined block rather than the
    // first group's window.
    assert_eq!(headers, vec!["@@ -17,167 +17,167 @@".to_owned()]);
    let shown_lines = shown(&all).0;
    assert_eq!(shown_lines.first().copied(), Some(17));
    assert_eq!(shown_lines.last().copied(), Some(183));
    assert_eq!(shown_lines.len(), 167, "no gap inside the block");
    // The gaps at the ends are untouched.
    assert!(gap_hidden(&gaps, 0) > 0 && gap_hidden(&gaps, 2) > 0);
}

#[test]
fn a_file_with_no_hunks_opens_from_both_ends() {
    let same: String = (1..=100).map(|i| format!("line {i}\n")).collect();
    let bytes = same.as_bytes();
    let closed = render_file(&HL, Some(bytes), Some(bytes), None, &opts(false));
    let (lines, gaps) = shown(&closed);
    assert!(lines.is_empty());
    assert_eq!(gaps, vec![(Gap::new(0), 100)]);
    let opened = render_file(&HL, Some(bytes), Some(bytes), None, &expanded(0, 5, 5));
    let (lines, gaps) = shown(&opened);
    check_invariants(&opened, Some(bytes), Some(bytes));
    assert_eq!(gap_hidden(&gaps, 0), 90);
    assert_eq!(lines, vec![1, 2, 3, 4, 5, 96, 97, 98, 99, 100]);
}
