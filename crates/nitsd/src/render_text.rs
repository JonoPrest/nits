//! Plain-text renderings of protocol values for the CLI and MCP tool results. Agents read
//! text, so a diff comes back looking like `git diff` with line numbers
//! rather than as row JSON.

use std::fmt::Write as _;

use nits_protocol::{Cell, FileRenderHeader, LineRange, RenderChunk, RenderContent, Row};

/// A rendered diff or blob as numbered text.
#[must_use]
pub fn render(header: &FileRenderHeader, chunks: &[RenderChunk]) -> String {
    if let nits_protocol::RenderTarget::Diff {
        change: nits_protocol::ChangeKind::Submodule { change },
    } = &header.target
    {
        return submodule(change);
    }
    let mut out = String::new();
    if let Some(summary) = metadata(&header.target) {
        let _ = writeln!(out, "{summary}");
    }
    if matches!(header.content, RenderContent::Binary) {
        out.push_str("(binary file)\n");
        return out;
    }
    for chunk in chunks {
        for row in &chunk.rows {
            match row {
                Row::HunkHeader { text } => line(&mut out, "", "", '@', text),
                Row::Context { left, right } => {
                    line(&mut out, &no(left), &no(right), ' ', &right.text);
                }
                Row::Removed { left } => line(&mut out, &no(left), "", '-', &left.text),
                Row::Added { right } => line(&mut out, "", &no(right), '+', &right.text),
                Row::Modified { left, right } => {
                    line(&mut out, &no(left), "", '-', &left.text);
                    line(&mut out, "", &no(right), '+', &right.text);
                }
                Row::Expander { hidden, .. } => {
                    line(&mut out, "", "", '~', &format!("{hidden} unchanged lines"));
                }
                Row::WhitespaceOnly => {
                    line(&mut out, "", "", '~', "whitespace-only change");
                }
            }
        }
    }
    out
}

/// A blob as `lineno│text` — blob renders only produce `Context` rows.
#[must_use]
pub fn render_blob(header: &FileRenderHeader, chunks: &[RenderChunk]) -> String {
    render_blob_selected(header, chunks, None)
}

/// Select an inclusive source range, retaining the blob's absolute line numbers.
/// This bounds formatted output; the caller still collects the full blob render.
/// Binary blobs retain the same placeholder as [`render_blob`].
#[must_use]
pub fn render_blob_range(
    header: &FileRenderHeader,
    chunks: &[RenderChunk],
    lines: LineRange,
) -> String {
    render_blob_selected(header, chunks, Some(lines))
}

fn render_blob_selected(
    header: &FileRenderHeader,
    chunks: &[RenderChunk],
    lines: Option<LineRange>,
) -> String {
    if let nits_protocol::RenderTarget::Diff {
        change: nits_protocol::ChangeKind::Submodule { change },
    } = &header.target
    {
        return submodule(change);
    }
    let mut out = String::new();
    if matches!(header.content, RenderContent::Binary) {
        out.push_str("(binary file)\n");
        return out;
    }
    for chunk in chunks {
        for row in &chunk.rows {
            if let Row::Context { right, .. } = row
                && lines.is_none_or(|range| range.contains(right.line_no))
            {
                let _ = writeln!(out, "{:>5}│{}", right.line_no.get(), right.text);
            }
        }
    }
    out
}

/// Tracked Git mode; regular file modes are not arbitrary filesystem permissions.
#[must_use]
pub fn mode(mode: nits_protocol::BlobMode) -> &'static str {
    use nits_protocol::BlobMode;
    match mode {
        BlobMode::Regular => "100644",
        BlobMode::Executable => "100755 (executable)",
        BlobMode::Symlink => "120000 (symlink)",
        BlobMode::Unknown => "unknown Git mode",
    }
}

/// Metadata remains visible even when a diff contains no changed source lines.
#[must_use]
pub fn metadata(target: &nits_protocol::RenderTarget) -> Option<String> {
    use nits_protocol::{BlobMode, ChangeKind, RenderTarget};
    match target {
        RenderTarget::Blob { entry } => Some(mode(entry.mode).into()),
        RenderTarget::Diff { change } => match change {
            ChangeKind::Added { new } => Some(format!("Added {}", mode(new.mode))),
            ChangeKind::Deleted { old } => Some(format!("Removed {}", mode(old.mode))),
            ChangeKind::Modified { old, new } | ChangeKind::Renamed { old, new, .. } => {
                if old.mode != new.mode {
                    Some(format!("{} → {}", mode(old.mode), mode(new.mode)))
                } else if old.mode == BlobMode::Unknown {
                    Some("Git mode unknown (historical)".into())
                } else {
                    None
                }
            }
            ChangeKind::Submodule { .. } => None,
        },
    }
}

fn no(c: &Cell) -> String {
    c.line_no.get().to_string()
}

fn line(out: &mut String, old: &str, new: &str, mark: char, text: &str) {
    let _ = writeln!(out, "{old:>5} {new:>5} {mark}{text}");
}

/// Gitlinks name commits, so these identities are metadata rather than source lines.
#[must_use]
pub fn submodule(change: &nits_protocol::SubmoduleChange) -> String {
    use nits_protocol::SubmoduleChange;
    match change {
        SubmoduleChange::Added { new } => format!("Submodule added\nnew commit: {new}\n"),
        SubmoduleChange::Deleted { old } => format!("Submodule removed\nold commit: {old}\n"),
        SubmoduleChange::Updated { old, new } => {
            format!("Submodule updated\nold commit: {old}\nnew commit: {new}\n")
        }
        SubmoduleChange::Renamed { from, old, new } => {
            format!("Submodule renamed from {from}\nold commit: {old}\nnew commit: {new}\n")
        }
        SubmoduleChange::BlobToSubmodule { old, new } => {
            format!(
                "Blob replaced by submodule\nold blob: {} ({})\nnew commit: {new}\n",
                old.oid,
                mode(old.mode)
            )
        }
        SubmoduleChange::SubmoduleToBlob { old, new } => {
            format!(
                "Submodule replaced by blob\nold commit: {old}\nnew blob: {} ({})\n",
                new.oid,
                mode(new.mode)
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nits_protocol::{BlobOid, LineNo, RenderOpts, RenderTarget, RepoId, RepoPath};
    use nits_review_core::render::{Highlighter, render_blob as blob};

    #[test]
    fn mode_and_type_metadata_survives_zero_hunks_binary_and_unknown_history() {
        use nits_protocol::{BlobEntry, BlobMode, ChangeKind};
        let entry = |mode| BlobEntry {
            oid: BlobOid::from_bytes([1; 20]),
            mode,
        };
        for (old, new, expected) in [
            (
                BlobMode::Regular,
                BlobMode::Executable,
                "100644 → 100755 (executable)",
            ),
            (
                BlobMode::Executable,
                BlobMode::Regular,
                "100755 (executable) → 100644",
            ),
            (
                BlobMode::Regular,
                BlobMode::Symlink,
                "100644 → 120000 (symlink)",
            ),
            (
                BlobMode::Symlink,
                BlobMode::Regular,
                "120000 (symlink) → 100644",
            ),
            (
                BlobMode::Unknown,
                BlobMode::Unknown,
                "Git mode unknown (historical)",
            ),
        ] {
            let target = RenderTarget::Diff {
                change: ChangeKind::Modified {
                    old: entry(old),
                    new: entry(new),
                },
            };
            assert_eq!(metadata(&target).as_deref(), Some(expected));
            for content in [
                blob(&Highlighter::new(), b"", None).content,
                RenderContent::Binary,
            ] {
                let header = FileRenderHeader {
                    repo_id: RepoId::from_parts(1, 1),
                    path: RepoPath::new("entry").unwrap(),
                    target: target.clone(),
                    opts: RenderOpts::default(),
                    lang: None,
                    content,
                };
                let text = render(&header, &[]);
                assert!(text.starts_with(&format!("{expected}\n")));
                if matches!(header.content, RenderContent::Binary) {
                    assert!(text.contains("(binary file)"));
                } else {
                    assert_eq!(text, format!("{expected}\n"));
                }
            }
        }
        for mode in [BlobMode::Regular, BlobMode::Executable, BlobMode::Symlink] {
            let target = RenderTarget::Diff {
                change: ChangeKind::Modified {
                    old: entry(mode),
                    new: entry(mode),
                },
            };
            assert!(metadata(&target).is_none());
        }
    }

    #[test]
    fn bounded_blob_text_matches_full_text_with_absolute_line_numbers() {
        let source = "source\n".repeat(1015);
        let rendered = blob(&Highlighter::new(), source.as_bytes(), None);
        let chunks: Vec<_> = rendered.chunks().collect();
        let header = FileRenderHeader {
            repo_id: RepoId::from_parts(1, 1),
            path: RepoPath::new("source.txt").unwrap(),
            target: RenderTarget::Blob {
                entry: nits_protocol::BlobEntry {
                    oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                        .parse::<BlobOid>()
                        .unwrap(),
                    mode: nits_protocol::BlobMode::Regular,
                },
            },
            opts: RenderOpts::default(),
            lang: None,
            content: rendered.content,
        };
        let range = LineRange::new(LineNo::new(995).unwrap(), LineNo::new(1020).unwrap()).unwrap();
        let full = render_blob(&header, &chunks);
        let expected = full.lines().skip(994).collect::<Vec<_>>().join("\n") + "\n";
        assert_eq!(render_blob_range(&header, &chunks, range), expected);
        assert!(expected.starts_with("  995│"));
        assert!(expected.ends_with(" 1015│source\n"));
        assert_eq!(
            render_blob_range(
                &header,
                &chunks,
                LineRange::single(LineNo::new(1016).unwrap())
            ),
            ""
        );
    }
}
