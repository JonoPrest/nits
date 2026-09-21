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

fn no(c: &Cell) -> String {
    c.line_no.get().to_string()
}

fn line(out: &mut String, old: &str, new: &str, mark: char, text: &str) {
    let _ = writeln!(out, "{old:>5} {new:>5} {mark}{text}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use nits_protocol::{BlobOid, LineNo, RenderOpts, RenderTarget, RepoId, RepoPath};
    use nits_review_core::render::{Highlighter, render_blob as blob};

    #[test]
    fn bounded_blob_text_matches_full_text_with_absolute_line_numbers() {
        let source = "source\n".repeat(1015);
        let rendered = blob(&Highlighter::new(), source.as_bytes(), None);
        let chunks: Vec<_> = rendered.chunks().collect();
        let header = FileRenderHeader {
            repo_id: RepoId::from_parts(1, 1),
            path: RepoPath::new("source.txt").unwrap(),
            target: RenderTarget::Blob {
                oid: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .parse::<BlobOid>()
                    .unwrap(),
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
            format!("Blob replaced by submodule\nold blob: {old}\nnew commit: {new}\n")
        }
        SubmoduleChange::SubmoduleToBlob { old, new } => {
            format!("Submodule replaced by blob\nold commit: {old}\nnew blob: {new}\n")
        }
    }
}
