//! Minimal unified-diff application for suggestion patches.
//!
//! Accepts the hunk format carried by `CommentKind::Suggestion`: zero or
//! more `@@ -a,b +c,d @@` hunks with ` `, `-`, `+` lines. Context and removed
//! lines must match exactly at the stated position (no fuzz).

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PatchError {
    #[error("malformed hunk header: {0:?}")]
    Header(String),
    #[error("unexpected line in hunk: {0:?}")]
    Line(String),
    #[error("hunk at old line {line} does not match: expected {expected:?}, found {found:?}")]
    Mismatch {
        line: usize,
        expected: String,
        found: Option<String>,
    },
}

pub fn apply(original: &[u8], patch: &str) -> Result<Vec<u8>, PatchError> {
    let text = String::from_utf8_lossy(original);
    let had_trailing_nl = text.ends_with('\n');
    let old_lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<String> = Vec::new();
    let mut cursor = 0usize; // next old line index not yet copied

    let mut lines = patch.lines().peekable();
    while let Some(line) = lines.next() {
        if line.starts_with("---") || line.starts_with("+++") || line.is_empty() {
            continue;
        }
        let Some(header) = line.strip_prefix("@@ ") else {
            return Err(PatchError::Line(line.to_owned()));
        };
        let old_start =
            parse_old_start(header).ok_or_else(|| PatchError::Header(line.to_owned()))?;
        let target = old_start.saturating_sub(1);
        if target < cursor || target > old_lines.len() {
            return Err(PatchError::Header(line.to_owned()));
        }
        out.extend(old_lines[cursor..target].iter().map(|s| (*s).to_owned()));
        cursor = target;
        while let Some(&body) = lines.peek() {
            if body.starts_with("@@ ") {
                break;
            }
            lines.next();
            if body == "\\ No newline at end of file" {
                continue;
            }
            let mut chars = body.chars();
            let tag = chars.next();
            let content = chars.as_str();
            match tag {
                Some(' ' | '-') => {
                    let found = old_lines.get(cursor).copied();
                    if found != Some(content) {
                        return Err(PatchError::Mismatch {
                            line: cursor + 1,
                            expected: content.to_owned(),
                            found: found.map(str::to_owned),
                        });
                    }
                    if tag == Some(' ') {
                        out.push(content.to_owned());
                    }
                    cursor += 1;
                }
                Some('+') => out.push(content.to_owned()),
                None => {
                    // A blank patch line is an empty context line.
                    let found = old_lines.get(cursor).copied();
                    if found != Some("") {
                        return Err(PatchError::Mismatch {
                            line: cursor + 1,
                            expected: String::new(),
                            found: found.map(str::to_owned),
                        });
                    }
                    out.push(String::new());
                    cursor += 1;
                }
                Some(_) => return Err(PatchError::Line(body.to_owned())),
            }
        }
    }
    out.extend(old_lines[cursor..].iter().map(|s| (*s).to_owned()));
    let mut joined = out.join("\n");
    if had_trailing_nl || (original.is_empty() && !joined.is_empty()) {
        joined.push('\n');
    }
    Ok(joined.into_bytes())
}

fn parse_old_start(header: &str) -> Option<usize> {
    // "-a,b +c,d @@..." or "-a +c @@"
    let rest = header.strip_prefix('-')?;
    let num: String = rest.chars().take_while(char::is_ascii_digit).collect();
    num.parse().ok()
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
        let out = apply(b"a\nb", "@@ -2,1 +2,1 @@\n-b\n+c\n").unwrap();
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
            let expected = body.lines().next().unwrap().to_owned();
            prop_assert_eq!(
                apply(b"a\nb\nc\n", &format!("@@ -2,1 +2,1 @@\n{body}")),
                Err(PatchError::Line(expected))
            );
        }
    }
}
