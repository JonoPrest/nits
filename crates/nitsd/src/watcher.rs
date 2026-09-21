//! File watcher (plan 2.3). One `notify` watcher per attached repo; a burst
//! of writes is debounced into one pass that snapshots the working tree,
//! broadcasts a `TreeDelta` if the tree changed, and re-resolves every open
//! review with a working-tree target on that repo (which emits
//! `ReviewTargetsResolved` only when something actually moved).
//!
//! Git HEAD/index/ref changes also invalidate resolved provenance. Only relevant
//! metadata is considered: snapshot indexes, objects and Nits retention refs
//! must not feed back into the watcher that created them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use nits_protocol::{Author, ClientId, ClientSeq, EventBody, RepoId, TreeOid};
use notify::{RecursiveMode, Watcher as _};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::daemon::Daemon;

/// Quiet period after the last filesystem event before a repo is processed.
pub const DEBOUNCE: Duration = Duration::from_millis(150);

/// Handle to the watcher task; cancelling `shutdown` stops it.
#[derive(Debug)]
pub struct Watcher {
    shutdown: CancellationToken,
}

impl Watcher {
    /// Watch every attached repo now and follow attach/detach events.
    pub fn start(daemon: Arc<Daemon>) -> Self {
        let shutdown = CancellationToken::new();
        tokio::spawn(run(daemon, shutdown.clone()));
        Self { shutdown }
    }

    pub fn stop(&self) {
        self.shutdown.cancel();
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Watched {
    _watcher: notify::RecommendedWatcher,
    /// Last tree we told subscribers about; `None` until the first pass.
    last_tree: Option<TreeOid>,
}

async fn run(daemon: Arc<Daemon>, shutdown: CancellationToken) {
    let (fs_tx, mut fs_rx) = mpsc::unbounded_channel::<RepoId>();
    let mut watched: HashMap<RepoId, Watched> = HashMap::new();
    let mut pending: HashMap<RepoId, tokio::time::Instant> = HashMap::new();
    let mut events = daemon.subscribe();

    sync_repos(&daemon, &fs_tx, &mut watched).await;

    loop {
        let next_due = pending.values().min().copied();
        let sleep = async {
            match next_due {
                Some(t) => tokio::time::sleep_until(t).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            () = shutdown.cancelled() => return,
            Some(repo_id) = fs_rx.recv() => {
                pending.insert(repo_id, tokio::time::Instant::now() + DEBOUNCE);
            }
            ev = events.recv() => {
                if let Ok(ev) = ev
                    && matches!(ev.body, EventBody::RepoAttached { .. } | EventBody::RepoDetached { .. })
                {
                    sync_repos(&daemon, &fs_tx, &mut watched).await;
                }
            }
            () = sleep => {
                let now = tokio::time::Instant::now();
                let due: Vec<RepoId> = pending
                    .iter()
                    .filter(|(_, t)| **t <= now)
                    .map(|(r, _)| *r)
                    .collect();
                for repo_id in due {
                    pending.remove(&repo_id);
                    if let Some(w) = watched.get_mut(&repo_id) {
                        process(&daemon, repo_id, &mut w.last_tree).await;
                    }
                }
            }
        }
    }
}

/// Start watching newly attached repos and drop detached ones.
async fn sync_repos(
    daemon: &Arc<Daemon>,
    fs_tx: &mpsc::UnboundedSender<RepoId>,
    watched: &mut HashMap<RepoId, Watched>,
) {
    let repos: Vec<(RepoId, PathBuf)> = match daemon.read(nits_review_core::Core::workspaces).await
    {
        Ok(ws) => ws
            .into_iter()
            .flat_map(|w| w.repos)
            .map(|r| (r.id, PathBuf::from(r.path)))
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, "listing repos for the watcher");
            return;
        }
    };
    watched.retain(|id, _| repos.iter().any(|(r, _)| r == id));
    for (id, path) in repos {
        if watched.contains_key(&id) {
            continue;
        }
        match watch_one(id, &path, fs_tx.clone()) {
            Ok(w) => {
                let mut w = Watched {
                    _watcher: w,
                    last_tree: None,
                };
                // Seed the baseline so the first real change yields a delta.
                process(daemon, id, &mut w.last_tree).await;
                watched.insert(id, w);
            }
            Err(e) => {
                tracing::warn!(repo = %id, path = %path.display(), error = %e, "watch failed");
            }
        }
    }
}

fn watch_one(
    id: RepoId,
    path: &Path,
    tx: mpsc::UnboundedSender<RepoId>,
) -> notify::Result<notify::RecommendedWatcher> {
    let paths =
        WatchPaths::discover(path).map_err(|error| notify::Error::generic(&error.to_string()))?;
    let callback_paths = paths.clone();
    let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(ev) = res else { return };
        // Reads while resolving must not schedule another resolution.
        if !matches!(ev.kind, notify::EventKind::Access(_))
            && (ev.need_rescan() || ev.paths.iter().any(|p| callback_paths.relevant(p)))
        {
            let _ = tx.send(id);
        }
    })?;
    w.watch(&paths.root, RecursiveMode::Recursive)?;
    if !paths.metadata.common.starts_with(&paths.root) {
        w.watch(&paths.metadata.common, RecursiveMode::Recursive)?;
    }
    if !paths.metadata.worktree.starts_with(&paths.root)
        && !paths.metadata.worktree.starts_with(&paths.metadata.common)
    {
        w.watch(&paths.metadata.worktree, RecursiveMode::Recursive)?;
    }
    Ok(w)
}

#[derive(Clone)]
struct WatchPaths {
    root: PathBuf,
    metadata: nits_review_core::git::GitMetadataPaths,
}

impl WatchPaths {
    fn discover(path: &Path) -> Result<Self, nits_review_core::git::GitError> {
        let repo = nits_review_core::git::Repo::open(path)?;
        Ok(Self {
            root: std::fs::canonicalize(repo.workdir())?,
            metadata: repo.metadata_paths()?,
        })
    }

    fn relevant(&self, path: &Path) -> bool {
        if let Ok(relative) = path.strip_prefix(&self.metadata.worktree)
            && matches!(
                relative.to_str(),
                Some("HEAD" | "index" | "config.worktree")
            )
        {
            return true;
        }
        if let Ok(relative) = path.strip_prefix(&self.metadata.common) {
            return matches!(
                relative.to_str(),
                Some("config" | "packed-refs" | "info/exclude")
            ) || (relative.starts_with("refs") && !relative.starts_with("refs/nits"))
                || relative.starts_with("reftable");
        }
        if path.starts_with(&self.metadata.worktree) {
            return false;
        }
        path.starts_with(&self.root)
    }
}

/// Snapshot the working tree, broadcast a delta if it moved, and re-resolve
/// dependent reviews.
async fn process(daemon: &Arc<Daemon>, repo_id: RepoId, last_tree: &mut Option<TreeOid>) {
    let tree = match daemon.read(move |c| c.working_tree(repo_id)).await {
        Ok(r) => r.tree,
        Err(e) => {
            tracing::warn!(repo = %repo_id, error = %e, "working tree snapshot failed");
            return;
        }
    };
    let previous = last_tree.replace(tree);
    if let Some(from) = previous
        && from != tree
    {
        match daemon
            .read(move |c| c.tree_delta(repo_id, from, tree))
            .await
        {
            Ok(delta) => daemon.broadcast_delta(delta),
            Err(e) => tracing::warn!(repo = %repo_id, error = %e, "tree delta failed"),
        }
    }
    // An unchanged content tree says nothing about HEAD, dirty paths, the
    // selected branch or a moving base ref. Core compares all resolved fields.
    let reviews = match daemon.read(move |c| c.working_tree_reviews(repo_id)).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "listing working-tree reviews");
            return;
        }
    };
    for review_id in reviews {
        let ctx = Daemon::ctx(
            daemon_author(),
            ClientId::from_parts(0, 0),
            ClientSeq::new(0),
        );
        if let Err(e) = daemon
            .write(move |c| c.refresh_working_tree_review(&ctx, review_id, repo_id))
            .await
        {
            tracing::warn!(review = %review_id, error = %e, "resolve after file change failed");
        }
    }
}

/// Author for events the daemon raises itself.
#[must_use]
pub fn daemon_author() -> Author {
    Author::Daemon {
        machine: gethostname::gethostname().to_string_lossy().into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nits_test_support::{RepoBuilder, files};

    #[test]
    fn metadata_filter_tracks_shared_and_private_git_state_without_own_writes() {
        let repo = RepoBuilder::new()
            .commit("base", files!["a.txt" => "a\n"])
            .build()
            .unwrap();
        let linked = tempfile::tempdir().unwrap();
        let checkout = linked.path().join("checkout");
        repo.git(&[
            "worktree",
            "add",
            "-b",
            "linked",
            checkout.to_str().unwrap(),
        ])
        .unwrap();
        for checkout in [repo.path(), checkout.as_path()] {
            let paths = WatchPaths::discover(checkout).unwrap();
            for path in ["HEAD", "index", "config.worktree"] {
                assert!(
                    paths.relevant(&paths.metadata.worktree.join(path)),
                    "{path}"
                );
            }
            for path in [
                "packed-refs",
                "config",
                "info/exclude",
                "refs/heads/main",
                "refs/tags/v1",
                "refs/remotes/origin/main",
            ] {
                assert!(paths.relevant(&paths.metadata.common.join(path)), "{path}");
            }
            for path in [
                "nits-index-123/index",
                "nits-index-123/index.lock",
                "objects/ab/cdef",
                "logs/HEAD",
            ] {
                assert!(
                    !paths.relevant(&paths.metadata.worktree.join(path)),
                    "{path}"
                );
            }
            for path in [
                "refs/nits",
                "refs/nits/reviews/a/trees/b",
                "refs/nits/reviews/a/commits/c.lock",
                "logs/refs/nits/reviews/a",
            ] {
                assert!(!paths.relevant(&paths.metadata.common.join(path)), "{path}");
            }
            assert!(paths.relevant(&paths.root.join("a.txt")));
        }
    }
}
