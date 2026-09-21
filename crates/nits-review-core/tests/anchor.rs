#![allow(clippy::format_collect, clippy::cast_possible_truncation)] // test data builders
//! Table-driven anchoring cases (§4.5) plus the "edits outside the range
//! never outdate" property.

use nits_protocol::{Anchor, BlobOid, CommentState, LineNo, LineRange, RepoId, RepoPath, Side};
use nits_review_core::anchor::{CONTEXT_LINES, PathFate, Reanchor, context_hash, reanchor};
use nits_review_core::render::lines_of;
use proptest::prelude::*;

fn blob(n: u8) -> BlobOid {
    BlobOid::from_bytes([n; 20])
}
fn path(s: &str) -> RepoPath {
    RepoPath::new(s).unwrap()
}
fn range(a: u32, b: u32) -> LineRange {
    LineRange::new(LineNo::new(a).unwrap(), LineNo::new(b).unwrap()).unwrap()
}

/// A `Lines` anchor on `old` with the hash computed from its content.
fn lines_anchor(old: &str, a: u32, b: u32) -> Anchor {
    Anchor::Lines {
        repo_id: RepoId::nil(),
        path: path("f.rs"),
        side: Side::Head,
        blob_oid: blob(1),
        lines: range(a, b),
        context_hash: context_hash(&lines_of(old.as_bytes()), range(a, b)),
    }
}

fn run(anchor: &Anchor, state: &CommentState, fate: &PathFate, old: &str, new: &str) -> Reanchor {
    reanchor(
        anchor,
        state,
        fate,
        |_| Some(old.as_bytes().to_vec()),
        |_| Some(new.as_bytes().to_vec()),
    )
}

fn present(b: BlobOid) -> PathFate {
    PathFate::Present {
        path: path("f.rs"),
        blob: b,
    }
}

fn numbered(n: u32) -> String {
    (1..=n).map(|i| format!("line {i}\n")).collect()
}

fn mapped_lines(r: &Reanchor) -> Option<(u32, u32)> {
    match r {
        Reanchor::Moved {
            anchor: Anchor::Lines { lines, .. },
            state: CommentState::Live,
        } => Some((lines.start().get(), lines.end().get())),
        _ => None,
    }
}

#[test]
fn unchanged_blob_is_unchanged() {
    let old = numbered(20);
    let a = lines_anchor(&old, 10, 12);
    assert_eq!(
        run(&a, &CommentState::Live, &present(blob(1)), &old, &old),
        Reanchor::Unchanged
    );
}

#[test]
fn lines_shifted_by_insert_above_stay_live() {
    let old = numbered(20);
    let new = format!("inserted\ninserted\n{old}");
    let a = lines_anchor(&old, 10, 12);
    let r = run(&a, &CommentState::Live, &present(blob(2)), &old, &new);
    assert_eq!(mapped_lines(&r), Some((12, 14)));
}

#[test]
fn lines_shifted_by_delete_above_stay_live() {
    let old = numbered(20);
    let new: String = old.lines().skip(3).map(|l| format!("{l}\n")).collect();
    let a = lines_anchor(&old, 10, 12);
    let r = run(&a, &CommentState::Live, &present(blob(2)), &old, &new);
    assert_eq!(mapped_lines(&r), Some((7, 9)));
}

#[test]
fn edit_within_range_is_outdated_keeping_last_good() {
    let old = numbered(20);
    let new = old.replace("line 11\n", "line ELEVEN\n");
    let a = lines_anchor(&old, 10, 12);
    let r = run(&a, &CommentState::Live, &present(blob(2)), &old, &new);
    assert_eq!(
        r,
        Reanchor::Moved {
            anchor: a.clone(),
            state: CommentState::Outdated {
                last_good_anchor: a
            }
        }
    );
}

#[test]
fn edit_in_surrounding_context_is_outdated() {
    let old = numbered(20);
    let new = old.replace("line 8\n", "line EIGHT\n"); // 2 above the range: inside ±3
    let a = lines_anchor(&old, 10, 12);
    let r = run(&a, &CommentState::Live, &present(blob(2)), &old, &new);
    assert!(
        matches!(
            r,
            Reanchor::Moved {
                state: CommentState::Outdated { .. },
                ..
            }
        ),
        "{r:?}"
    );
}

#[test]
fn edit_beyond_context_stays_live_and_unshifted() {
    let old = numbered(20);
    let new = old.replace("line 2\n", "line TWO\n");
    let a = lines_anchor(&old, 10, 12);
    let r = run(&a, &CommentState::Live, &present(blob(2)), &old, &new);
    assert_eq!(mapped_lines(&r), Some((10, 12)));
}

#[test]
fn deleted_file_is_outdated_and_renamed_file_follows() {
    let old = numbered(20);
    let a = lines_anchor(&old, 10, 12);
    let r = run(&a, &CommentState::Live, &PathFate::Gone, &old, &old);
    assert!(matches!(
        r,
        Reanchor::Moved {
            state: CommentState::Outdated { .. },
            ..
        }
    ));

    let renamed = PathFate::Present {
        path: path("g.rs"),
        blob: blob(1),
    };
    let r = run(&a, &CommentState::Live, &renamed, &old, &old);
    let Reanchor::Moved {
        anchor: Anchor::Lines { path: p, lines, .. },
        state: CommentState::Live,
    } = r
    else {
        panic!("{r:?}");
    };
    assert_eq!(p.as_str(), "g.rs");
    assert_eq!((lines.start().get(), lines.end().get()), (10, 12));
}

#[test]
fn outdated_comment_revives_when_content_returns() {
    let old = numbered(20);
    let a = lines_anchor(&old, 10, 12);
    let outdated = CommentState::Outdated {
        last_good_anchor: a.clone(),
    };
    // Still gone: nothing to do.
    assert_eq!(
        run(&a, &outdated, &PathFate::Gone, &old, &old),
        Reanchor::Unchanged
    );
    // Same blob is back at the path: live again at the original anchor.
    let r = run(&a, &outdated, &present(blob(1)), &old, &old);
    assert_eq!(
        r,
        Reanchor::Moved {
            anchor: a.clone(),
            state: CommentState::Live
        }
    );
    // A shifted version is back: live at the shifted range.
    let new = format!("x\n{old}");
    let r = run(&a, &outdated, &present(blob(3)), &old, &new);
    assert_eq!(mapped_lines(&r), Some((11, 13)));
}

#[test]
fn file_anchor_follows_blob_and_goes_outdated_when_gone() {
    let a = Anchor::File {
        repo_id: RepoId::nil(),
        path: path("f.rs"),
        blob_oid: blob(1),
    };
    assert_eq!(
        run(&a, &CommentState::Live, &present(blob(1)), "", ""),
        Reanchor::Unchanged
    );
    let r = run(&a, &CommentState::Live, &present(blob(2)), "", "");
    assert_eq!(
        r,
        Reanchor::Moved {
            anchor: Anchor::File {
                repo_id: RepoId::nil(),
                path: path("f.rs"),
                blob_oid: blob(2)
            },
            state: CommentState::Live
        }
    );
    let r = run(&a, &CommentState::Live, &PathFate::Gone, "", "");
    assert_eq!(
        r,
        Reanchor::Moved {
            anchor: a.clone(),
            state: CommentState::Outdated {
                last_good_anchor: a
            }
        }
    );
}

#[test]
fn review_anchor_and_deleted_comments_never_move() {
    let r = run(
        &Anchor::Review,
        &CommentState::Live,
        &PathFate::Gone,
        "",
        "",
    );
    assert_eq!(r, Reanchor::Unchanged);
    let old = numbered(5);
    let a = lines_anchor(&old, 1, 2);
    assert_eq!(
        run(&a, &CommentState::Deleted, &PathFate::Gone, &old, ""),
        Reanchor::Unchanged
    );
}

#[derive(Debug, Clone)]
enum Edit {
    Insert { at: u32, count: u32 },
    Delete { at: u32, count: u32 },
    Replace { at: u32 },
}

fn apply_edits(
    lines: &[String],
    edits: &[Edit],
    protect: std::ops::RangeInclusive<u32>,
) -> Vec<String> {
    // Apply edits from the bottom up so earlier indexes stay valid; skip any
    // that would touch the protected 1-based range. Among edits at the same
    // index a delete goes last, otherwise a later replace/insert at that
    // index would land on the line that slid up into it.
    let mut out = lines.to_vec();
    let mut sorted = edits.to_vec();
    sorted.sort_by_key(|e| {
        let (at, rank) = match e {
            Edit::Replace { at } => (*at, 0),
            Edit::Insert { at, .. } => (*at, 1),
            Edit::Delete { at, .. } => (*at, 2),
        };
        (std::cmp::Reverse(at), rank)
    });
    // Keep only pairwise-disjoint edits (processed bottom-up): an edit
    // whose original range reaches into an already-applied higher edit's
    // position would act on shifted content, making the original-line
    // protect checks unsound — e.g. Delete@6 then Delete@5x2 eating a
    // protected context line (see the proptest regressions).
    let mut floor = u32::MAX;
    sorted.retain(|e| {
        let (at, reach) = match e {
            Edit::Insert { at, .. } => (*at, *at),
            Edit::Replace { at } => (*at, at + 1),
            Edit::Delete { at, count } => (*at, at + count),
        };
        if reach <= floor {
            floor = at;
            true
        } else {
            false
        }
    });
    for e in sorted {
        match e {
            Edit::Insert { at, count } => {
                let at = at.min(out.len() as u32);
                // The inserted line lands at 1-based position at+1; that is
                // inside the window when it is between its ends inclusive.
                if protect.contains(&(at + 1)) {
                    continue;
                }
                for k in 0..count {
                    out.insert(at as usize, format!("ins {k}"));
                }
            }
            Edit::Delete { at, count } => {
                let at = at as usize;
                let end = (at + count as usize).min(out.len());
                if at >= end {
                    continue;
                }
                let touches = (at as u32 + 1..=end as u32).any(|l| protect.contains(&l));
                if touches {
                    continue;
                }
                out.drain(at..end);
            }
            Edit::Replace { at } => {
                let at = at as usize;
                if at >= out.len() || protect.contains(&(at as u32 + 1)) {
                    continue;
                }
                out[at] = format!("replaced {at}");
            }
        }
    }
    out
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    #[test]
    fn edits_outside_the_context_window_never_outdate(
        n in 12u32..40,
        start in 1u32..30,
        len in 0u32..3,
        edits in prop::collection::vec(
            prop_oneof![
                (0u32..40, 1u32..3).prop_map(|(at, count)| Edit::Insert { at, count }),
                (0u32..40, 1u32..3).prop_map(|(at, count)| Edit::Delete { at, count }),
                (0u32..40).prop_map(|at| Edit::Replace { at }),
            ],
            0..4,
        ),
    ) {
        let start = start.min(n);
        let end = (start + len).min(n);
        let old_lines: Vec<String> = (1..=n).map(|i| format!("line {i}")).collect();
        // Inserts just past EOF still land inside the ±CONTEXT window, so do not clamp to n.
        let protect = start.saturating_sub(CONTEXT_LINES).max(1)..=(end + CONTEXT_LINES);
        let new_lines = apply_edits(&old_lines, &edits, protect);
        let old: String = old_lines.iter().map(|l| format!("{l}\n")).collect();
        let new: String = new_lines.iter().map(|l| format!("{l}\n")).collect();
        let a = lines_anchor(&old, start, end);
        let r = run(&a, &CommentState::Live, &present(blob(2)), &old, &new);
        match r {
            Reanchor::Unchanged => prop_assert_eq!(old, new),
            Reanchor::Moved { state: CommentState::Live, anchor: Anchor::Lines { lines, .. } } => {
                // Mapped lines carry the same text.
                for (k, i) in (lines.start().index()..=lines.end().index()).enumerate() {
                    prop_assert_eq!(&new_lines[i as usize], &old_lines[(start - 1) as usize + k]);
                }
            }
            other @ Reanchor::Moved { .. } => prop_assert!(false, "unexpected {other:?} for edits {edits:?}"),
        }
    }
}

#[test]
fn terminator_only_edits_keep_line_ranges_and_live_anchors() {
    for old in ["first\nlast", "first\nlast\n", "first\r\nlast\r\n"] {
        for new in ["first\nlast", "first\nlast\n", "first\r\nlast\r\n"] {
            if old == new {
                continue;
            }
            let anchor = lines_anchor(old, 1, 2);
            let result = run(&anchor, &CommentState::Live, &present(blob(2)), old, new);
            assert_eq!(mapped_lines(&result), Some((1, 2)), "{old:?} -> {new:?}");
        }
    }
}
