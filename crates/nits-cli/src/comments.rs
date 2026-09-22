//! CLI parsing and presentation of the Core's complete conversation query.

use std::fmt::Write as _;

use clap::{Args, ValueEnum};
use nits_protocol::{
    Anchor, Author, CommentListing, CommentQuery, CommentState, CommentThreadStatus, RepoId,
    RepoPath, ReviewId, Seq, ThreadId, ThreadResolution,
};

#[derive(Debug, Args)]
pub(crate) struct ListArgs {
    #[command(flatten)]
    review: super::ReviewArg,
    /// Only actionable open findings (excludes deleted roots).
    #[arg(long, conflicts_with = "status")]
    open: bool,
    #[arg(long, value_enum)]
    status: Option<StatusArg>,
    #[arg(long, value_name = "ID")]
    thread: Option<ThreadId>,
    /// Exact repository-relative path on any comment in the thread.
    #[arg(long, value_parser = parse_path)]
    path: Option<RepoPath>,
    /// Disambiguate identical paths in different repositories.
    #[arg(long)]
    repo: Option<RepoId>,
    /// Exact human or agent name on any comment in the thread.
    #[arg(long)]
    author: Option<String>,
    /// Complete threads with activity after this committed sequence (exclusive).
    #[arg(long, value_name = "SEQ")]
    since: Option<u64>,
    /// One line per comment: id, status, anchor and first body line.
    #[arg(long)]
    oneline: bool,
}

fn parse_path(value: &str) -> Result<RepoPath, nits_protocol::InvariantError> {
    RepoPath::new(value)
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum StatusArg {
    Open,
    Resolved,
    Deferred,
    Informational,
    Deleted,
}

impl From<StatusArg> for CommentThreadStatus {
    fn from(value: StatusArg) -> Self {
        match value {
            StatusArg::Open => Self::Open,
            StatusArg::Resolved => Self::Resolved,
            StatusArg::Deferred => Self::Deferred,
            StatusArg::Informational => Self::Informational,
            StatusArg::Deleted => Self::Deleted,
        }
    }
}

impl ListArgs {
    pub(crate) fn into_query(self) -> (ReviewId, CommentQuery, bool) {
        (
            self.review.0,
            CommentQuery {
                status: if self.open {
                    Some(CommentThreadStatus::Open)
                } else {
                    self.status.map(Into::into)
                },
                thread_id: self.thread,
                path: self.path,
                repo_id: self.repo,
                author: self.author,
                since: self.since.map(Seq::new),
            },
            self.oneline,
        )
    }
}

fn status_text(status: CommentThreadStatus) -> &'static str {
    match status {
        CommentThreadStatus::Open => "open",
        CommentThreadStatus::Resolved => "resolved",
        CommentThreadStatus::Deferred => "deferred (unfixed)",
        CommentThreadStatus::Informational => "informational",
        CommentThreadStatus::Deleted => "deleted",
    }
}

pub(crate) fn text(listing: &CommentListing, oneline: bool) -> String {
    let mut out = String::new();
    for thread in &listing.threads {
        let status = status_text(thread.status);
        if !oneline {
            let _ = writeln!(out, "thread {} [{status}]", thread.id);
            if let ThreadResolution::Deferred {
                reason,
                tracking_url,
                by,
                at,
            } = &thread.resolution
            {
                let _ = writeln!(out, "  Deferred by {by:?} at {at:?}: {reason}");
                if let Some(url) = tracking_url {
                    let _ = writeln!(out, "  Follow-up: {url}");
                }
            }
        }
        for comment in &thread.comments {
            let who = match &comment.author {
                Author::Human { name, .. } | Author::Agent { name, .. } => name.as_str(),
                Author::Daemon { .. } => "daemon",
            };
            let state = match comment.state {
                CommentState::Live => "",
                CommentState::Outdated { .. } => " [outdated]",
                CommentState::Deleted => " [deleted]",
            };
            let anchor = match &comment.anchor {
                Anchor::Review => "review".into(),
                Anchor::File { repo_id, .. } | Anchor::Lines { repo_id, .. } => {
                    format!("{} [repo {repo_id}]", super::anchor_text(&comment.anchor))
                }
            };
            let body = if matches!(comment.state, CommentState::Deleted) {
                "[deleted comment]"
            } else {
                &comment.body
            };
            if oneline {
                let _ = writeln!(
                    out,
                    "{} [{status}]{state} thread {} {who} @ {anchor}: {}",
                    comment.id,
                    thread.id,
                    body.lines().next().unwrap_or("")
                );
            } else {
                let _ = writeln!(out, "  {} {who}{state} @ {anchor}", comment.id);
                for line in body.split('\n') {
                    let _ = writeln!(out, "    {line}");
                }
                out.push('\n');
            }
        }
    }
    let s = &listing.summary;
    let threads = if s.threads == 1 { "thread" } else { "threads" };
    let comments = if s.comments == 1 {
        "comment"
    } else {
        "comments"
    };
    let _ = writeln!(
        out,
        "{} {threads}: {} open, {} resolved, {} deferred, {} informational, {} deleted; {} {comments} ({} deleted); seq {}",
        s.threads,
        s.open,
        s.resolved,
        s.deferred,
        s.informational,
        s.deleted,
        s.comments,
        s.deleted_comments,
        listing.seq
    );
    out
}
