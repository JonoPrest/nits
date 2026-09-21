//! Strict, byte-preserving unified-diff application for suggestion patches.
//!
//! Accepts one or more `@@ -a,b +c,d @@` hunks, optionally preceded by a
//! `---`/`+++` file-header pair. Both ranges and body counts must agree; context
//! and removed lines match exactly, including CRLF. Body lines end in LF;
//! `\ No newline at end of file` removes that LF from the preceding source
//! line. No fuzz, newline normalization, binary patches or multi-file patches.

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PatchError {
    #[error("malformed hunk header or range: {0:?}")]
    Header(String),
    #[error("unexpected line in hunk: {0:?}")]
    Line(String),
    #[error("patch must contain at least one hunk")]
    Empty,
    #[error("hunk body does not match the counts in {0:?}")]
    Count(String),
    #[error(
        "patch body line must end in LF; use a no-newline marker for an unterminated source line"
    )]
    UnterminatedPatchLine,
    #[error(
        "a line without a final newline must be nonempty and only appear at the end of the resulting file"
    )]
    MisplacedEndOfFile,
    #[error("hunk at old line {line} does not match: expected {expected:?}, found {found:?}")]
    Mismatch {
        line: usize,
        expected: String,
        found: Option<String>,
    },
}

/// A validated unified-diff range, represented as a zero-based line offset.
/// Git's zero-count ranges point *after* the stated line; nonempty ranges
/// point at the stated one-based line. Construction also rules out overflow.
#[derive(Debug)]
struct HunkRange {
    offset: usize,
    count: usize,
}

impl HunkRange {
    fn parse(value: &str) -> Option<Self> {
        let (start, count) = match value.split_once(',') {
            Some((start, count)) => (number(start)?, number(count)?),
            None => (number(value)?, 1),
        };
        let offset = if count == 0 {
            start
        } else {
            start.checked_sub(1)?
        };
        offset.checked_add(count)?;
        Some(Self { offset, count })
    }
}

fn number(value: &str) -> Option<usize> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

#[derive(Debug, Clone, Copy)]
enum LineKind {
    Context,
    Remove,
    Add,
}

#[derive(Debug)]
struct BodyLine<'a> {
    kind: LineKind,
    /// Exact source bytes, including LF unless followed by a no-newline marker.
    bytes: &'a [u8],
}

#[derive(Debug)]
struct Hunk<'a> {
    header: &'a str,
    old: HunkRange,
    new: HunkRange,
    lines: Vec<BodyLine<'a>>,
}

impl<'a> Hunk<'a> {
    fn header(header: &'a str) -> Result<Self, PatchError> {
        let parse = || {
            let (old, rest) = header.strip_prefix("@@ -")?.split_once(" +")?;
            let (new, suffix) = rest.split_once(" @@")?;
            if !suffix.is_empty() && !suffix.starts_with(' ') {
                return None;
            }
            let old = HunkRange::parse(old)?;
            let new = HunkRange::parse(new)?;
            if old.count == 0 && new.count == 0 {
                return None;
            }
            Some(Self {
                header,
                old,
                new,
                lines: Vec::new(),
            })
        };
        parse().ok_or_else(|| PatchError::Header(header.to_owned()))
    }

    fn validate_counts(&self) -> Result<(), PatchError> {
        let mut old = 0;
        let mut new = 0;
        for line in &self.lines {
            match line.kind {
                LineKind::Context => {
                    old += 1;
                    new += 1;
                }
                LineKind::Remove => old += 1,
                LineKind::Add => new += 1,
            }
        }
        if old != self.old.count || new != self.new.count {
            return Err(PatchError::Count(self.header.to_owned()));
        }
        Ok(())
    }
}

#[derive(Debug)]
struct Patch<'a> {
    hunks: Vec<Hunk<'a>>,
}

impl<'a> TryFrom<&'a str> for Patch<'a> {
    type Error = PatchError;

    fn try_from(patch: &'a str) -> Result<Self, Self::Error> {
        let mut lines = patch.split_inclusive('\n').peekable();
        if lines.peek().is_some_and(|line| line.starts_with("--- ")) {
            lines.next();
            if !lines.next().is_some_and(|line| line.starts_with("+++ ")) {
                return Err(PatchError::Header("expected +++ file header".into()));
            }
        }
        let mut hunks = Vec::new();
        while let Some(header) = lines.next() {
            let header = header.strip_suffix('\n').unwrap_or(header);
            let mut hunk = Hunk::header(header)?;
            while let Some(&body) = lines.peek() {
                if body.starts_with("@@ ") {
                    break;
                }
                lines.next();
                let mut chars = body.chars();
                let kind = match chars.next() {
                    Some(' ') => LineKind::Context,
                    Some('-') => LineKind::Remove,
                    Some('+') => LineKind::Add,
                    Some(_) | None => {
                        return Err(PatchError::Line(body.trim_end_matches('\n').to_owned()));
                    }
                };
                let content = chars.as_str();
                if !content.ends_with('\n') {
                    return Err(PatchError::UnterminatedPatchLine);
                }
                let bytes = if lines.peek().is_some_and(|line| {
                    line.trim_end_matches('\n') == "\\ No newline at end of file"
                }) {
                    lines.next();
                    content
                        .strip_suffix('\n')
                        .ok_or(PatchError::UnterminatedPatchLine)?
                        .as_bytes()
                } else {
                    content.as_bytes()
                };
                if bytes.is_empty() {
                    return Err(PatchError::MisplacedEndOfFile);
                }
                hunk.lines.push(BodyLine { kind, bytes });
            }
            hunk.validate_counts()?;
            hunks.push(hunk);
        }
        if hunks.is_empty() {
            return Err(PatchError::Empty);
        }
        Ok(Self { hunks })
    }
}

pub fn apply(original: &[u8], patch: &str) -> Result<Vec<u8>, PatchError> {
    let patch = Patch::try_from(patch)?;
    apply_parsed(original, &patch)
}

/// The proposed bytes and their exact, validated source-line presentation.
#[derive(Debug)]
pub struct PatchPreview {
    pub hunks: Vec<nits_protocol::SuggestionHunk>,
    pub result: Vec<u8>,
}

/// Uses the application parser and original-byte checks. It neither writes
/// objects nor reads today's checkout, so stale suggestions remain inspectable.
pub fn preview(original: &[u8], source: &str) -> Result<PatchPreview, PatchError> {
    use nits_protocol::{LineEnding, LineNo, SuggestionHunk, SuggestionLine, SuggestionLineKind};

    let patch = Patch::try_from(source)?;
    let result = apply_parsed(original, &patch)?;
    let mut hunks = Vec::with_capacity(patch.hunks.len());
    for hunk in patch.hunks {
        let line_no = |offset: usize| {
            offset
                .checked_add(1)
                .and_then(|line| u32::try_from(line).ok())
                .and_then(LineNo::new)
                .ok_or_else(|| PatchError::Header(hunk.header.to_owned()))
        };
        let mut old = hunk.old.offset;
        let mut new = hunk.new.offset;
        let mut lines = Vec::with_capacity(hunk.lines.len());
        for line in hunk.lines {
            let kind = match line.kind {
                LineKind::Context => {
                    let kind = SuggestionLineKind::Context {
                        old: line_no(old)?,
                        new: line_no(new)?,
                    };
                    old += 1;
                    new += 1;
                    kind
                }
                LineKind::Remove => {
                    let kind = SuggestionLineKind::Remove { old: line_no(old)? };
                    old += 1;
                    kind
                }
                LineKind::Add => {
                    let kind = SuggestionLineKind::Add { new: line_no(new)? };
                    new += 1;
                    kind
                }
            };
            let (text, ending) = if let Some(text) = line.bytes.strip_suffix(b"\r\n") {
                (text, LineEnding::CrLf)
            } else if let Some(text) = line.bytes.strip_suffix(b"\n") {
                (text, LineEnding::Lf)
            } else {
                (line.bytes, LineEnding::Missing)
            };
            // Body lines are slices of the validated UTF-8 patch string.
            let text = std::str::from_utf8(text)
                .map_err(|error| PatchError::Line(error.to_string()))?
                .to_owned();
            lines.push(SuggestionLine { kind, text, ending });
        }
        hunks.push(SuggestionHunk {
            header: hunk.header.to_owned(),
            lines,
        });
    }
    Ok(PatchPreview { hunks, result })
}

fn apply_parsed(original: &[u8], patch: &Patch<'_>) -> Result<Vec<u8>, PatchError> {
    let old_lines: Vec<&[u8]> = original.split_inclusive(|&b| b == b'\n').collect();
    let mut out = Vec::new();
    let mut cursor = 0;
    for hunk in &patch.hunks {
        let untouched = old_lines
            .get(cursor..hunk.old.offset)
            .ok_or_else(|| PatchError::Header(hunk.header.to_owned()))?;
        out.extend_from_slice(untouched);
        // The new range includes the cumulative line delta of earlier hunks.
        if out.len() != hunk.new.offset {
            return Err(PatchError::Header(hunk.header.to_owned()));
        }
        cursor = hunk.old.offset;
        for line in &hunk.lines {
            match line.kind {
                LineKind::Context | LineKind::Remove => {
                    let found = old_lines.get(cursor).copied();
                    if found != Some(line.bytes) {
                        return Err(PatchError::Mismatch {
                            line: cursor + 1,
                            expected: String::from_utf8_lossy(line.bytes).into_owned(),
                            found: found.map(|bytes| String::from_utf8_lossy(bytes).into_owned()),
                        });
                    }
                    if matches!(line.kind, LineKind::Context) {
                        out.push(line.bytes);
                    }
                    cursor += 1;
                }
                LineKind::Add => out.push(line.bytes),
            }
        }
    }
    out.extend_from_slice(
        old_lines
            .get(cursor..)
            .ok_or(PatchError::MisplacedEndOfFile)?,
    );
    // An insertion cannot silently join with an unterminated preceding line,
    // and a no-newline marker cannot manufacture a non-final source line.
    if out.iter().rev().skip(1).any(|line| !line.ends_with(b"\n")) {
        return Err(PatchError::MisplacedEndOfFile);
    }
    Ok(out.concat())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn replaces_a_line() {
        let out = apply(b"a\nb\nc\n", "@@ -2,1 +2,1 @@\n-b\n+B\n").unwrap();
        assert_eq!(out, b"a\nB\nc\n");
    }

    #[test]
    fn inserts_and_keeps_context() {
        let out = apply(b"a\nb\nc\n", "@@ -1,3 +1,4 @@\n a\n b\n+b2\n c\n").unwrap();
        assert_eq!(out, b"a\nb\nb2\nc\n");
    }

    #[test]
    fn two_hunks() {
        let src = b"1\n2\n3\n4\n5\n6\n7\n8\n";
        let out = apply(
            src,
            "@@ -1,1 +1,1 @@\n-1\n+one\n@@ -8,1 +8,1 @@\n-8\n+eight\n",
        )
        .unwrap();
        assert_eq!(out, b"one\n2\n3\n4\n5\n6\n7\neight\n");
    }

    #[test]
    fn mismatch_is_an_error() {
        let err = apply(b"a\nb\n", "@@ -1,1 +1,1 @@\n-x\n+y\n").unwrap_err();
        assert!(matches!(err, PatchError::Mismatch { line: 1, .. }), "{err}");
    }

    #[test]
    fn preserves_missing_trailing_newline() {
        let out = apply(
            b"a\nb",
            "@@ -2,1 +2,1 @@\n-b\n\\ No newline at end of file\n+c\n\\ No newline at end of file\n",
        )
        .unwrap();
        assert_eq!(out, b"a\nc");
    }

    #[test]
    fn unicode_content_is_allowed_but_unicode_prefixes_are_rejected() {
        let out = apply("é\n".as_bytes(), "@@ -1,1 +1,1 @@\n-é\n+🦀\n").unwrap();
        assert_eq!(out, "🦀\n".as_bytes());
        for body in ["é", "中", "🦀"] {
            assert_eq!(
                apply(b"a\nb\nc\n", &format!("@@ -2,1 +2,1 @@\n{body}\n")),
                Err(PatchError::Line(body.to_owned()))
            );
        }
    }

    #[test]
    fn retains_non_utf8_bytes_outside_the_edit() {
        let original = b"\xff\r\nb\n\xfe";
        assert_eq!(
            apply(original, "@@ -2 +2 @@\n-b\n+B\n").unwrap(),
            b"\xff\r\nB\n\xfe"
        );
    }

    #[test]
    fn validates_range_syntax_and_counts() {
        for patch in [
            "@@ -1 +1 @@trailing\n-a\n+b\n",
            "@@ -1,1,1 +1 @@\n-a\n+b\n",
            "@@ --1 +1 @@\n-a\n+b\n",
            "@@ -1 +0 @@\n-a\n+b\n",
            "@@ -1,0 +1,0 @@\n",
            "@@ -1,2 +1 @@\n-a\n+b\n",
            "@@ -1 +1,2 @@\n-a\n+b\n",
            "@@ -1 +1 @@\n-a\n+b\n+extra\n",
            "@@ -1 +1 @@\n-a\n",
            "@@ -1 +1\n-a\n+b\n",
            "@@ -1 +1 @@\n\\ No newline at end of file\n-a\n+b\n",
            "@@ -1 +1 @@\n-a\n+b\n\\ No newline at end of file\n\\ No newline at end of file\n",
        ] {
            assert!(apply(b"a\n", patch).is_err(), "{patch:?}");
        }
        let overflowing = format!("@@ -{},2 +1 @@\n-a\n+b\n", usize::MAX);
        assert!(matches!(
            apply(b"a\n", &overflowing),
            Err(PatchError::Header(_))
        ));
        assert_eq!(apply(b"a\n", ""), Err(PatchError::Empty));
    }

    #[test]
    fn newline_markers_are_side_specific_and_only_at_eof() {
        let invalid: &[(&[u8], &str)] = &[
            (b"a", "@@ -1 +1 @@\n-a\n+b\n"), // Missing old-side marker.
            (
                b"a\n",
                "@@ -1 +1 @@\n-a\n\\ No newline at end of file\n+b\n",
            ),
            (
                b"a\nb\n",
                "@@ -1 +1 @@\n-a\n+A\n\\ No newline at end of file\n",
            ),
            (b"a", "@@ -1,0 +2 @@\n+b\n"), // Cannot append after an unterminated line.
            (b"", "@@ -0,0 +1 @@\n+\n\\ No newline at end of file\n"),
            (
                b"a\n",
                "@@ -1,0 +2 @@\n+b\n\\ No newline at end of file\n@@ -1,0 +3 @@\n+c\n",
            ),
        ];
        for &(original, patch) in invalid {
            assert!(apply(original, patch).is_err(), "{patch:?}");
        }
    }

    proptest! {
        #[test]
        fn arbitrary_utf8_patches_do_not_panic(
            original in prop::collection::vec(any::<u8>(), 0..1024),
            patch in prop::collection::vec(any::<char>(), 0..1024),
        ) {
            let patch: String = patch.into_iter().collect();
            let _ = apply(&original, &patch);
            // Raw random text usually fails before a hunk. Also exercise the
            // body parser with an in-range header on every generated input.
            let _ = apply(&original, &format!("@@ -1,1 +1,1 @@\n{patch}"));
        }

        #[test]
        fn every_non_ascii_hunk_prefix_is_a_typed_line_error(
            prefix in any::<char>().prop_filter("non-ASCII prefix", |c| !c.is_ascii()),
            suffix in prop::collection::vec(any::<char>(), 0..256),
        ) {
            let body: String = std::iter::once(prefix).chain(suffix).collect();
            let expected = body.split('\n').next().unwrap().to_owned();
            prop_assert_eq!(
                apply(b"a\nb\nc\n", &format!("@@ -2,1 +2,1 @@\n{body}")),
                Err(PatchError::Line(expected))
            );
        }
    }
}
