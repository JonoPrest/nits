//! User-supplied Git revisions and named fetch remotes, parsed at the boundary.

use core::{fmt, str::FromStr};
use serde::{Deserialize, Serialize};

/// One Git revision expression. Git resolves it to exactly one commit beside
/// the checkout; trees, blobs, ranges and unavailable objects are rejected there.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(try_from = "String", into = "String")]
pub struct RevisionExpr(String);

impl TryFrom<String> for RevisionExpr {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.trim().is_empty() || value.chars().any(char::is_control) {
            return Err("a Git revision must be nonempty and contain no control characters".into());
        }
        Ok(Self(value))
    }
}

/// A configured Git remote name, also safe as a remote-tracking ref prefix.
/// URLs are deliberately excluded: fetching uses the checkout's named remotes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(try_from = "String", into = "String")]
pub struct RemoteName(String);

impl Default for RemoteName {
    fn default() -> Self {
        Self("origin".into())
    }
}

impl TryFrom<String> for RemoteName {
    type Error = String;

    // Git ref components forbid the literal case-sensitive `.lock` suffix.
    #[allow(clippy::case_sensitive_file_extension_comparisons)]
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty()
            || value.starts_with('-')
            || value.ends_with('.')
            || value.contains("..")
            || value.contains("@{")
            || value
                .chars()
                .any(|c| c.is_control() || c.is_whitespace() || "~^:?*[\\".contains(c))
            || value
                .split('/')
                .any(|part| part.is_empty() || part.starts_with('.') || part.ends_with(".lock"))
        {
            return Err(
                "expected a configured Git remote name, such as origin (not a URL or refspec)"
                    .into(),
            );
        }
        Ok(Self(value))
    }
}

/// Implement the shared string boundary for validated Git input newtypes.
macro_rules! string_input {
    ($ty:ty) => {
        impl $ty {
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl FromStr for $ty {
            type Err = String;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::try_from(value.to_owned())
            }
        }
        impl From<$ty> for String {
            fn from(value: $ty) -> Self {
                value.0
            }
        }
        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}
string_input!(RevisionExpr);
string_input!(RemoteName);

impl FromStr for crate::RefSpec {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let value = value.trim();
        match value.to_ascii_lowercase().as_str() {
            "worktree" | "working-tree" | "wt" => return Ok(Self::WorkingTree),
            "head" => return Ok(Self::Head),
            "upstream" | "@{upstream}" | "@{u}" => return Ok(Self::Upstream),
            _ => {}
        }
        for (prefix, branch) in [("branch:", true), ("tag:", false)] {
            if let Some(name) = value.strip_prefix(prefix) {
                let name = name.parse::<RevisionExpr>()?.to_string();
                return Ok(if branch {
                    Self::Branch { name }
                } else {
                    Self::Tag { name }
                });
            }
        }
        if let Some(oid) = value.strip_prefix("commit:") {
            return oid
                .parse()
                .map(|oid| Self::Commit { oid })
                .map_err(|error| format!("invalid commit OID: {error}"));
        }
        if value.len() == 40 && value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return value
                .parse()
                .map(|oid| Self::Commit { oid })
                .map_err(|error| format!("invalid commit OID: {error}"));
        }
        value
            .parse()
            .map(|expression| Self::Revision { expression })
    }
}

impl TryFrom<crate::RefSpec> for crate::BaseRefSpec {
    type Error = String;

    fn try_from(value: crate::RefSpec) -> Result<Self, Self::Error> {
        match value {
            crate::RefSpec::Branch { name } => Ok(Self::Branch { name }),
            crate::RefSpec::Tag { name } => Ok(Self::Tag { name }),
            crate::RefSpec::Commit { oid } => Ok(Self::Commit { oid }),
            crate::RefSpec::Revision { expression } => Ok(Self::Revision { expression }),
            crate::RefSpec::Upstream => Ok(Self::Upstream),
            crate::RefSpec::Head => Ok(Self::Head),
            crate::RefSpec::WorkingTree => Err(
                "a review base cannot be worktree; select a Git revision, HEAD or upstream".into(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_and_remote_inputs_validate_on_the_wire() {
        for value in ["", "\0", "main\n", "\t"] {
            assert!(serde_json::from_value::<RevisionExpr>(serde_json::json!(value)).is_err());
        }
        for value in ["origin", "review/team", "upstream-2"] {
            assert_eq!(value.parse::<RemoteName>().unwrap().as_str(), value);
        }
        for value in [
            "",
            "-all",
            "../outside",
            "file:/repo",
            "main:HEAD",
            "a b",
            "a*",
            "a\\b",
            "a//b",
            ".hidden",
            "foo.lock",
            "x@{1}",
        ] {
            assert!(
                serde_json::from_value::<RemoteName>(serde_json::json!(value)).is_err(),
                "{value}"
            );
        }
    }

    #[test]
    fn input_preserves_explicit_names_and_accepts_git_expressions() {
        for value in [
            "main",
            "origin/main",
            "refs/remotes/origin/main",
            "f00ba12",
            "v1.0",
            "HEAD~1",
            ":/fix typo",
        ] {
            assert_eq!(
                value.parse::<crate::RefSpec>().unwrap(),
                crate::RefSpec::Revision {
                    expression: value.parse().unwrap()
                }
            );
        }
        assert_eq!(
            "branch:main".parse::<crate::RefSpec>().unwrap(),
            crate::RefSpec::Branch {
                name: "main".into()
            }
        );
        assert_eq!(
            "tag:v1".parse::<crate::RefSpec>().unwrap(),
            crate::RefSpec::Tag { name: "v1".into() }
        );
        assert!("tag:".parse::<crate::RefSpec>().is_err());
        assert!(crate::BaseRefSpec::try_from(crate::RefSpec::WorkingTree).is_err());
    }
}
