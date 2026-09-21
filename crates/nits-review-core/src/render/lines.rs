//! Line splitting that keeps the original text and a whitespace-free key.

use nits_protocol::LineEnding;

/// One source line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// Text without its `\n` / `\r\n` terminator, lossily decoded.
    pub text: String,
    pub ending: LineEnding,
    /// `text` with all whitespace removed — the diff key under
    /// `ignore_whitespace`.
    pub normalised: String,
}

/// Split into lines. A trailing terminator does not produce an extra empty
/// line; a missing one still yields the last line.
pub fn split_lines(bytes: &[u8]) -> Vec<Line> {
    if bytes.is_empty() {
        return Vec::new();
    }
    bytes
        .split_inclusive(|b| *b == b'\n')
        .map(|raw| {
            let (raw, ending) = match raw.strip_suffix(b"\n") {
                Some(line) => match line.strip_suffix(b"\r") {
                    Some(line) => (line, LineEnding::CrLf),
                    None => (line, LineEnding::Lf),
                },
                None => (raw, LineEnding::Missing),
            };
            let text = String::from_utf8_lossy(raw).into_owned();
            let normalised = text.chars().filter(|c| !c.is_whitespace()).collect();
            Line {
                text,
                ending,
                normalised,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn texts(b: &[u8]) -> Vec<String> {
        split_lines(b).into_iter().map(|l| l.text).collect()
    }

    #[test]
    fn terminators() {
        assert_eq!(texts(b""), Vec::<String>::new());
        assert_eq!(texts(b"a\nb\n"), vec!["a", "b"]);
        assert_eq!(texts(b"a\nb"), vec!["a", "b"]);
        assert_eq!(texts(b"a\r\nb\r\n"), vec!["a", "b"]);
        assert_eq!(texts(b"\n"), vec![""]);
        assert_eq!(texts(b"a\n\n"), vec!["a", ""]);
    }

    #[test]
    fn normalised_strips_all_whitespace() {
        let l = &split_lines(b"  let x =\t1; \n")[0];
        assert_eq!(l.text, "  let x =\t1; ");
        assert_eq!(l.normalised, "letx=1;");
    }

    #[test]
    fn endings_are_bytes_not_display_whitespace() {
        let lines = split_lines(b"a\r\nb\nc\r");
        assert_eq!(
            lines
                .iter()
                .map(|l| (l.text.as_str(), l.ending))
                .collect::<Vec<_>>(),
            vec![
                ("a", LineEnding::CrLf),
                ("b", LineEnding::Lf),
                ("c\r", LineEnding::Missing)
            ]
        );
        assert_eq!(split_lines(b"\r\n")[0].ending, LineEnding::CrLf);
        assert_eq!(split_lines(b"\r")[0].text, "\r");
    }

    proptest! {
        #[test]
        fn utf8_source_reconstructs_exactly(source in any::<String>()) {
            let lines = split_lines(source.as_bytes());
            let mut rebuilt = String::new();
            for line in &lines {
                rebuilt.push_str(&line.text);
                rebuilt.push_str(line.ending.as_str());
            }
            prop_assert_eq!(&rebuilt, &source);
            prop_assert!(lines.iter().enumerate().all(|(i,line)| line.ending != LineEnding::Missing || i+1 == lines.len()));
            prop_assert_eq!(lines.len(), source.bytes().filter(|b| *b == b'\n').count() + usize::from(!source.is_empty() && !source.ends_with('\n')));
        }
    }
}
