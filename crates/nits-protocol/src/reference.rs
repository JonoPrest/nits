//! Portable references identify a daemon context and stable review/comment IDs.
//! They never contain a browser bridge's ephemeral HTTP port. Named contexts
//! resolve through the recipient's configuration; socket references are local
//! to the machine owning that socket.

use crate::{CommentId, CommentState, ReviewId, ReviewSnapshot, ThreadId};
use serde::{Deserialize, Serialize};
use std::{fmt, str::FromStr};

/// A context locator with a nonempty, control-free value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReferenceContext(String);

impl ReferenceContext {
    pub fn named(name: &str) -> Result<Self, ReferenceError> {
        Self::new("context", name)
    }
    pub fn socket(path: &str) -> Result<Self, ReferenceError> {
        Self::new("socket", path)
    }
    pub fn websocket(url: &str) -> Result<Self, ReferenceError> {
        Self::new("websocket", url)
    }
    fn new(kind: &str, value: &str) -> Result<Self, ReferenceError> {
        format!("{kind}/{}", encode(value)).parse()
    }
    pub fn locator(&self) -> Result<ReferenceLocator, ReferenceError> {
        let (kind, value) = self.0.split_once('/').ok_or(ReferenceError::Malformed)?;
        let value = decode(value)?;
        match kind {
            "context" => Ok(ReferenceLocator::Named(value)),
            "socket" => Ok(ReferenceLocator::Socket(value)),
            "websocket" => Ok(ReferenceLocator::WebSocket(value)),
            _ => Err(ReferenceError::Malformed),
        }
    }
}

/// Parsed only at the host boundary; the core treats context identity as opaque.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferenceLocator {
    Named(String),
    Socket(String),
    WebSocket(String),
}

impl FromStr for ReferenceContext {
    type Err = ReferenceError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (kind, value) = s.split_once('/').ok_or(ReferenceError::Malformed)?;
        if !matches!(kind, "context" | "socket" | "websocket") {
            return Err(ReferenceError::Malformed);
        }
        let decoded = decode(value)?;
        if decoded.is_empty() || decoded.trim() != decoded || decoded.chars().any(char::is_control)
        {
            return Err(ReferenceError::Malformed);
        }
        if (kind == "socket" && !decoded.starts_with('/'))
            || (kind == "websocket"
                && !(decoded.starts_with("ws://") || decoded.starts_with("wss://")))
        {
            return Err(ReferenceError::Malformed);
        }
        Ok(Self(format!("{kind}/{}", encode(&decoded))))
    }
}
impl TryFrom<String> for ReferenceContext {
    type Error = ReferenceError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}
impl From<ReferenceContext> for String {
    fn from(c: ReferenceContext) -> Self {
        c.0
    }
}
impl fmt::Display for ReferenceContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ReferenceTarget {
    Review,
    Thread { thread_id: ThreadId },
    Comment { comment_id: CommentId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ReviewReference {
    pub context: ReferenceContext,
    pub review_id: ReviewId,
    pub target: ReferenceTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReferenceError {
    #[error(
        "invalid nits reference; expected nits://context/<name>/review/<id>[/thread/<id>|/comment/<id>]"
    )]
    Malformed,
    #[error("reference names a different review")]
    WrongReview,
    #[error("referenced thread or comment is missing from this review")]
    Missing,
    #[error("referenced comment has been deleted")]
    Deleted,
}

impl ReviewReference {
    /// Query accepted by the browser bridge; its port is deliberately absent
    /// from the portable reference itself.
    pub fn browser_query(&self) -> String {
        format!("?reference={}", encode(&self.to_string()))
    }

    /// Resolve against a fresh snapshot, including resolved and outdated threads.
    /// Returns the root for a thread reference and the exact reply for a comment.
    pub fn resolve(&self, snapshot: &ReviewSnapshot) -> Result<Option<CommentId>, ReferenceError> {
        if snapshot.review.id != self.review_id {
            return Err(ReferenceError::WrongReview);
        }
        let comment_id = match self.target {
            ReferenceTarget::Review => return Ok(None),
            ReferenceTarget::Thread { thread_id } => {
                snapshot
                    .threads
                    .iter()
                    .find(|t| t.id == thread_id)
                    .ok_or(ReferenceError::Missing)?
                    .root
            }
            ReferenceTarget::Comment { comment_id } => comment_id,
        };
        let comment = snapshot
            .comments
            .iter()
            .find(|c| c.id == comment_id)
            .ok_or(ReferenceError::Missing)?;
        if comment.state == CommentState::Deleted {
            return Err(ReferenceError::Deleted);
        }
        let thread = snapshot
            .threads
            .iter()
            .find(|t| t.id == comment.thread_id)
            .ok_or(ReferenceError::Missing)?;
        let root = snapshot
            .comments
            .iter()
            .find(|c| c.id == thread.root)
            .ok_or(ReferenceError::Missing)?;
        if root.state == CommentState::Deleted {
            return Err(ReferenceError::Deleted);
        }
        Ok(Some(comment_id))
    }
}
impl FromStr for ReviewReference {
    type Err = ReferenceError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.len() > 16384 {
            return Err(ReferenceError::Malformed);
        }
        let rest = s.strip_prefix("nits://").ok_or(ReferenceError::Malformed)?;
        let parts: Vec<_> = rest.split('/').collect();
        if parts.len() != 4 && parts.len() != 6 {
            return Err(ReferenceError::Malformed);
        }
        if parts[2] != "review" {
            return Err(ReferenceError::Malformed);
        }
        let context = format!("{}/{}", parts[0], parts[1]).parse()?;
        let review_id = parts[3].parse().map_err(|_| ReferenceError::Malformed)?;
        let target = if parts.len() == 4 {
            ReferenceTarget::Review
        } else {
            match parts[4] {
                "thread" => ReferenceTarget::Thread {
                    thread_id: parts[5].parse().map_err(|_| ReferenceError::Malformed)?,
                },
                "comment" => ReferenceTarget::Comment {
                    comment_id: parts[5].parse().map_err(|_| ReferenceError::Malformed)?,
                },
                _ => return Err(ReferenceError::Malformed),
            }
        };
        Ok(Self {
            context,
            review_id,
            target,
        })
    }
}
impl fmt::Display for ReviewReference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "nits://{}/review/{}", self.context, self.review_id)?;
        match self.target {
            ReferenceTarget::Review => Ok(()),
            ReferenceTarget::Thread { thread_id } => write!(f, "/thread/{thread_id}"),
            ReferenceTarget::Comment { comment_id } => write!(f, "/comment/{comment_id}"),
        }
    }
}
impl TryFrom<String> for ReviewReference {
    type Error = ReferenceError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}
impl From<ReviewReference> for String {
    fn from(r: ReviewReference) -> Self {
        r.to_string()
    }
}

fn encode(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(b"0123456789ABCDEF"[usize::from(b >> 4)]));
            out.push(char::from(b"0123456789ABCDEF"[usize::from(b & 15)]));
        }
    }
    out
}
fn decode(value: &str) -> Result<String, ReferenceError> {
    let mut out = Vec::new();
    let mut bytes = value.bytes();
    while let Some(b) = bytes.next() {
        if b == b'%' {
            let h = bytes
                .next()
                .and_then(|b| char::from(b).to_digit(16))
                .ok_or(ReferenceError::Malformed)?;
            let l = bytes
                .next()
                .and_then(|b| char::from(b).to_digit(16))
                .ok_or(ReferenceError::Malformed)?;
            out.push(u8::try_from(h * 16 + l).map_err(|_| ReferenceError::Malformed)?);
        } else if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b);
        } else {
            return Err(ReferenceError::Malformed);
        }
    }
    String::from_utf8(out).map_err(|_| ReferenceError::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    proptest! {
        #[test]
        fn contexts_roundtrip(value in "[a-zA-Z0-9 /:?#%é]{1,80}") {
            let value = value.trim();
            if !value.is_empty() {
                let context = ReferenceContext::named(value).unwrap();
                let reference = ReviewReference { context, review_id: ReviewId::from_parts(1, 2), target: ReferenceTarget::Comment { comment_id: CommentId::from_parts(3, 4) } };
                prop_assert_eq!(reference.to_string().parse::<ReviewReference>().unwrap(), reference);
            }
        }
    }
    #[test]
    fn malformed_inputs_fail() {
        for s in [
            "",
            "http://localhost/?review=x",
            "nits://context/%/review/x",
            "nits://context/%0A/review/x",
            "nits://context/a/review/x",
            "nits://context/a/review/00000000010000000000000002?x",
            "nits://unknown/a/review/00000000010000000000000002",
        ] {
            assert!(s.parse::<ReviewReference>().is_err(), "{s}");
        }
    }
}
