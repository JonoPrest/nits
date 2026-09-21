//! File watcher (plan 2.3). One `notify` watcher per attached repo; a burst
//! of writes is debounced into one pass that snapshots the working tree,
//! broadcasts a `TreeDelta` if the tree changed, and re-resolves every open
//! review with a working-tree target on that repo (which emits
//! `ReviewTargetsResolved` only when something actually moved).
//!
//! Git HEAD/index/ref changes also invalidate resolved provenance. Only relevant
//! metadata is considered: snapshot indexes, objects and Nits retention refs
//! must not feed back into the watcher that created them.

use std::collections::{HashMap, HashSet};
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
    paths: WatchPaths,
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
                        // Index and checkout changes can initialize, remove or relocate
                        // a submodule whose metadata needs an external watch.
                        refresh_paths(w, repo_id, &fs_tx).await;
                        process(&daemon, repo_id, &mut w.last_tree).await;
                    }
                }
            }
        }
    }
}

/// Reconcile registrations with validated ownership, including legacy repair.
async fn sync_repos(
    daemon: &Arc<Daemon>,
    fs_tx: &mpsc::UnboundedSender<RepoId>,
    watched: &mut HashMap<RepoId, Watched>,
) {
    let repos = match daemon
        .read(|core| {
            let ids: HashSet<_> = core
                .workspaces()?
                .into_iter()
                .flat_map(|workspace| workspace.repos)
                .map(|repo| repo.id)
                .collect();
            let paths = ids
                .into_iter()
                .filter_map(|id| match core.repo_checkout_path(id) {
                    Ok(path) => Some((id, path)),
                    Err(error) => {
                        tracing::warn!(repo = %id, %error, "repository ownership unavailable for watching");
                        None
                    }
                })
                .collect::<HashMap<_, _>>();
            Ok(paths)
        })
        .await
    {
        Ok(repos) => repos,
        Err(e) => {
            tracing::warn!(error = %e, "listing repos for the watcher");
            return;
        }
    };
    // An ID can remain present after its old membership is detached. Keep a
    // registration only if it still watches that ID's canonical checkout.
    watched.retain(|id, watch| repos.get(id).is_some_and(|path| *path == watch.paths.root));
    for (id, path) in repos {
        if watched.contains_key(&id) {
            continue;
        }
        match discover_paths(path.clone())
            .await
            .and_then(|paths| watch_one(id, paths, fs_tx.clone()))
        {
            Ok(mut w) => {
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
    paths: WatchPaths,
    tx: mpsc::UnboundedSender<RepoId>,
) -> notify::Result<Watched> {
    let callback_paths = paths.clone();
    let mut w = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(ev) = res else { return };
        if callback_paths.relevant_event(&ev) {
            let _ = tx.send(id);
        }
    })?;
    for path in paths.watch_roots() {
        w.watch(&path, RecursiveMode::Recursive)?;
    }
    Ok(Watched {
        _watcher: w,
        paths,
        last_tree: None,
    })
}

async fn discover_paths(path: PathBuf) -> notify::Result<WatchPaths> {
    tokio::task::spawn_blocking(move || WatchPaths::discover(&path))
        .await
        .map_err(|error| notify::Error::generic(&error.to_string()))?
        .map_err(|error| notify::Error::generic(&error.to_string()))
}

async fn refresh_paths(watched: &mut Watched, id: RepoId, tx: &mpsc::UnboundedSender<RepoId>) {
    let updated = discover_paths(watched.paths.root.clone())
        .await
        .and_then(|paths| {
            if paths == watched.paths {
                Ok(None)
            } else {
                watch_one(id, paths, tx.clone()).map(Some)
            }
        });
    match updated {
        Ok(Some(mut replacement)) => {
            // Install the replacement before dropping the previous registration.
            replacement.last_tree = watched.last_tree;
            *watched = replacement;
        }
        Ok(None) => {}
        Err(error) => tracing::warn!(repo = %id, %error, "updating submodule watches"),
    }
}

#[derive(Clone, PartialEq, Eq)]
struct WatchPaths {
    root: PathBuf,
    metadata: nits_review_core::git::GitMetadataPaths,
    submodules: Vec<nits_review_core::git::GitMetadataPaths>,
}

impl WatchPaths {
    fn discover(path: &Path) -> Result<Self, nits_review_core::git::GitError> {
        let repo = nits_review_core::git::Repo::open(path)?;
        Ok(Self {
            root: std::fs::canonicalize(repo.workdir())?,
            metadata: repo.metadata_paths()?,
            submodules: repo.submodule_metadata_paths()?,
        })
    }

    fn all_metadata(&self) -> impl Iterator<Item = &nits_review_core::git::GitMetadataPaths> {
        std::iter::once(&self.metadata).chain(&self.submodules)
    }

    fn watch_roots(&self) -> Vec<PathBuf> {
        let mut candidates = vec![self.root.clone()];
        for metadata in self.all_metadata() {
            candidates.extend([metadata.worktree.clone(), metadata.common.clone()]);
        }
        candidates.sort_by_key(|path| path.components().count());
        let mut roots = Vec::new();
        for path in candidates {
            if !roots.iter().any(|root: &PathBuf| path.starts_with(root)) {
                roots.push(path);
            }
        }
        roots
    }

    fn relevant(&self, path: &Path) -> bool {
        // Check every repository first: a child's .git/modules HEAD is inside
        // the parent's otherwise-ignored Git directory.
        if self
            .all_metadata()
            .any(|metadata| metadata_relevant(metadata, path))
        {
            return true;
        }
        if self.all_metadata().any(|metadata| {
            path.starts_with(&metadata.worktree) || path.starts_with(&metadata.common)
        }) {
            return false;
        }
        path.starts_with(&self.root)
    }

    fn relevant_event(&self, event: &notify::Event) -> bool {
        // Reads while resolving must not schedule another resolution.
        if matches!(event.kind, notify::EventKind::Access(_)) {
            return false;
        }
        // `absorbgitdirs` replaces an old-form .git directory with a gitfile.
        // Observe metadata-root lifecycle events without admitting directory
        // content notifications caused by our own index/object writes.
        let relocated = matches!(
            event.kind,
            notify::EventKind::Create(_)
                | notify::EventKind::Remove(_)
                | notify::EventKind::Modify(notify::event::ModifyKind::Name(_))
        );
        event.need_rescan()
            || event.paths.iter().any(|path| {
                self.relevant(path)
                    || (relocated
                        && self
                            .all_metadata()
                            .any(|metadata| path == &metadata.worktree || path == &metadata.common))
            })
    }
}

fn metadata_relevant(metadata: &nits_review_core::git::GitMetadataPaths, path: &Path) -> bool {
    if let Ok(relative) = path.strip_prefix(&metadata.worktree)
        && matches!(
            relative.to_str(),
            Some("HEAD" | "index" | "config.worktree")
        )
    {
        return true;
    }
    if let Ok(relative) = path.strip_prefix(&metadata.common) {
        return matches!(
            relative.to_str(),
            Some("config" | "packed-refs" | "info/exclude")
        ) || (relative.starts_with("refs") && !relative.starts_with("refs/nits"))
            || relative.starts_with("reftable");
    }
    false
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
        let dependency = RepoBuilder::new()
            .commit("dependency", files!["lib.txt" => "source\n"])
            .build()
            .unwrap();
        repo.git(&[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "--",
            dependency.path().to_str().unwrap(),
            "dep",
        ])
        .unwrap();
        for checkout in [repo.path(), checkout.as_path()] {
            let paths = WatchPaths::discover(checkout).unwrap();
            for metadata in paths.all_metadata() {
                for root in [&metadata.worktree, &metadata.common] {
                    for kind in [
                        notify::EventKind::Create(notify::event::CreateKind::Any),
                        notify::EventKind::Remove(notify::event::RemoveKind::Any),
                        notify::EventKind::Modify(notify::event::ModifyKind::Name(
                            notify::event::RenameMode::Any,
                        )),
                    ] {
                        assert!(
                            paths.relevant_event(&notify::Event::new(kind).add_path(root.clone()))
                        );
                    }
                    for kind in [
                        notify::EventKind::Access(notify::event::AccessKind::Any),
                        notify::EventKind::Modify(notify::event::ModifyKind::Data(
                            notify::event::DataChange::Any,
                        )),
                    ] {
                        assert!(
                            !paths.relevant_event(&notify::Event::new(kind).add_path(root.clone()))
                        );
                    }
                }
                for path in ["HEAD", "index", "config.worktree"] {
                    assert!(paths.relevant(&metadata.worktree.join(path)), "{path}");
                }
                for path in [
                    "packed-refs",
                    "config",
                    "info/exclude",
                    "refs/heads/main",
                    "refs/tags/v1",
                    "refs/remotes/origin/main",
                ] {
                    assert!(paths.relevant(&metadata.common.join(path)), "{path}");
                }
                for path in [
                    "nits-index-123/index",
                    "nits-index-123/index.lock",
                    "objects/ab/cdef",
                    "logs/HEAD",
                ] {
                    assert!(!paths.relevant(&metadata.worktree.join(path)), "{path}");
                }
                for path in [
                    "refs/nits",
                    "refs/nits/reviews/a/trees/b",
                    "refs/nits/reviews/a/commits/c.lock",
                    "logs/refs/nits/reviews/a",
                ] {
                    assert!(!paths.relevant(&metadata.common.join(path)), "{path}");
                }
            }
            assert!(paths.relevant(&paths.root.join("a.txt")));
            if checkout == repo.path() {
                assert_eq!(
                    paths.submodules.len(),
                    1,
                    "absorbed submodule metadata is included"
                );
                assert_eq!(paths.watch_roots(), vec![paths.root.clone()]);
            }
        }
    }
}
