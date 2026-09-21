//! Git engine: resolve refs, read objects, list changes, snapshot trees and
//! the working tree.
//!
//! `gix` reads objects and commits; the `git` binary is used where gix is
//! still weaker (rename detection, working-tree snapshotting, `@{upstream}`
//! resolution). See `docs/ARCHITECTURE.md` §4.3.
//!
//! The working-tree snapshot is a *real* tree object: files are added to a
//! temporary index and written with `git write-tree`, so dirty blobs exist in
//! the object database and every later read goes through OIDs like any other
//! ref. Unchanged files keep their index OID.

use std::path::{Path, PathBuf};
use std::process::Command;

use gix::bstr::ByteSlice;
use gix::objs::tree::EntryKind as K;
use nits_protocol::{
    BlobOid, ChangeKind, CommitInfo, CommitOid, Oid, RefCandidate, RefSpec, RepoId, RepoPath,
    ResolvedRef, ResolvedSource, Sig, Timestamp, TreeDelta, TreeEntry, TreeEntryKind, TreeOid,
    TreeSnapshot,
};

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("cannot open repository at {path}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: Box<gix::open::Error>,
    },
    #[error("git {args:?} failed: {stderr}")]
    Command { args: Vec<String>, stderr: String },
    #[error("cannot resolve {rev}: {reason}")]
    Resolve { rev: String, reason: String },
    #[error(
        "cannot determine a default review base for {path}: tried the branch reflog, closest ancestor branches, origin/HEAD, init.defaultBranch, main/master/trunk/develop, and the sole local branch; pass --base explicitly"
    )]
    DefaultBase { path: PathBuf },
    #[error("object {oid} not found or unreadable: {reason}")]
    Object { oid: Oid, reason: String },
    #[error("unexpected git output: {0}")]
    Parse(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// A local branch name read from git's ref database.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct LocalBranch(String);

impl LocalBranch {
    fn new(name: impl Into<String>) -> Option<Self> {
        let name = name.into();
        (!name.is_empty()).then_some(Self(name))
    }

    fn as_str(&self) -> &str {
        &self.0
    }

    fn from_qualified(reference: &str) -> Option<Self> {
        reference.strip_prefix("refs/heads/").and_then(Self::new)
    }

    fn qualified(&self) -> String {
        format!("refs/heads/{}", self.0)
    }

    fn into_ref_spec(self) -> RefSpec {
        RefSpec::Branch { name: self.0 }
    }
}

/// A fully qualified remote-tracking branch from git's ref database or a
/// local branch's configured upstream. It may be absent in an unfetched repo.
#[derive(Debug)]
struct RemoteBranch(String);

impl RemoteBranch {
    fn from_qualified(reference: &str) -> Option<Self> {
        reference
            .strip_prefix("refs/remotes/")
            .filter(|name| !name.is_empty())
            .map(|_| Self(reference.to_owned()))
    }

    fn origin_counterpart(branch: &LocalBranch) -> Self {
        Self(format!("refs/remotes/origin/{}", branch.as_str()))
    }
}

/// A git repository, safe to share across threads.
pub struct Repo {
    inner: gix::ThreadSafeRepository,
    workdir: PathBuf,
}

/// Git stores linked-worktree HEAD/index state separately from shared refs.
#[derive(Debug, Clone)]
pub struct GitMetadataPaths {
    pub worktree: PathBuf,
    pub common: PathBuf,
}

impl std::fmt::Debug for Repo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Repo")
            .field("workdir", &self.workdir)
            .finish_non_exhaustive()
    }
}

fn oid_from_gix(id: gix::ObjectId) -> Option<Oid> {
    let bytes: [u8; 20] = id.as_bytes().try_into().ok()?;
    Some(Oid::from_bytes(bytes))
}

fn gix_id(oid: Oid) -> gix::ObjectId {
    gix::ObjectId::from_bytes_or_panic(oid.as_bytes())
}

/// Git's own heuristic: a NUL in the first 8000 bytes means binary.
#[must_use]
pub fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|b| *b == 0)
}

impl Repo {
    pub fn open(path: &Path) -> Result<Self, GitError> {
        let repo = gix::open(path).map_err(|e| GitError::Open {
            path: path.to_path_buf(),
            source: Box::new(e),
        })?;
        let workdir = repo
            .workdir()
            .ok_or_else(|| GitError::Open {
                path: path.to_path_buf(),
                source: Box::new(gix::open::Error::NotARepository {
                    source: gix::discover::is_git::Error::MissingHead,
                    path: path.to_path_buf(),
                }),
            })?
            .to_path_buf();
        Ok(Self {
            inner: repo.into_sync(),
            workdir,
        })
    }

    #[must_use]
    pub fn workdir(&self) -> &Path {
        &self.workdir
    }

    /// Actual metadata directories, including paths outside linked checkouts.
    pub fn metadata_paths(&self) -> Result<GitMetadataPaths, GitError> {
        let local = self.local();
        Ok(GitMetadataPaths {
            worktree: std::fs::canonicalize(local.git_dir())?,
            common: std::fs::canonicalize(local.common_dir())?,
        })
    }

    fn local(&self) -> gix::Repository {
        self.inner.to_thread_local()
    }

    /// Run `git` in the work dir with extra env; returns raw stdout.
    fn git(&self, args: &[&str], env: &[(&str, &Path)]) -> Result<Vec<u8>, GitError> {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.workdir)
            .envs(env.iter().map(|(k, v)| (*k, *v)))
            .output()?;
        if out.status.success() {
            Ok(out.stdout)
        } else {
            Err(GitError::Command {
                args: args.iter().map(|s| (*s).to_owned()).collect(),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            })
        }
    }

    /// Run a git command whose non-zero status means "not present" rather
    /// than an exceptional failure. Process-spawn errors remain errors.
    fn git_probe(&self, args: &[&str]) -> Result<Option<Vec<u8>>, GitError> {
        let out = Command::new("git")
            .args(args)
            .current_dir(&self.workdir)
            .output()?;
        Ok(out.status.success().then_some(out.stdout))
    }

    // ---- refs -------------------------------------------------------------

    /// Choose the revision a working-tree review should be based on.
    ///
    /// Git does not retain a durable parent-branch relationship, so this
    /// follows a deterministic ladder: a named reflog creation source, the
    /// closest local branch by merge-base distance, then a detected trunk.
    /// The checked-out trunk is handled before ancestor ranking so a feature
    /// branch created from it cannot be mistaken for its parent.
    /// Trunk is ranked at its remote-tracking merge-base with HEAD if that
    /// advances the local trunk, so unrelated sibling branches cannot beat
    /// a stale trunk. This reads existing refs; it never fetches or moves
    /// them, or overrides a named stack parent.
    pub fn default_base(&self) -> Result<RefSpec, GitError> {
        let branches = self.local_branches()?;
        let current = self.current_branch()?;
        let trunk = self.trunk_branch(&branches)?;

        if let Some(trunk_branch) = trunk.as_ref()
            && current.as_ref() == Some(trunk_branch)
        {
            return Ok(trunk_branch.clone().into_ref_spec());
        }

        if let Some(branch) = current.as_ref()
            && let Some(parent) = self.reflog_parent(branch, &branches)?
        {
            return self.parent_base(parent, trunk.as_ref());
        }

        if let Some(parent) = self.closest_branch(current.as_ref(), trunk.as_ref(), &branches)? {
            return self.parent_base(parent, trunk.as_ref());
        }

        let parent = trunk.clone().ok_or_else(|| GitError::DefaultBase {
            path: self.workdir.clone(),
        })?;
        self.parent_base(parent, trunk.as_ref())
    }

    fn parent_base(
        &self,
        parent: LocalBranch,
        trunk: Option<&LocalBranch>,
    ) -> Result<RefSpec, GitError> {
        if trunk == Some(&parent)
            && let Some(oid) = self.advanced_remote_trunk_base(&parent)?
        {
            return Ok(RefSpec::Commit { oid });
        }
        Ok(parent.into_ref_spec())
    }

    /// A remote trunk can advance independently after the feature's last
    /// rebase. Pin the shared ancestor, never the remote tip: reviewing
    /// against the tip would include changes the feature has not incorporated.
    /// Require the shared ancestor to descend from local trunk so a diverged
    /// local branch's commits cannot silently disappear from the base.
    fn advanced_remote_trunk_base(
        &self,
        trunk: &LocalBranch,
    ) -> Result<Option<CommitOid>, GitError> {
        let trunk_ref = trunk.qualified();
        let upstream = self.git(&["for-each-ref", "--format=%(upstream)", &trunk_ref], &[])?;
        let remote = RemoteBranch::from_qualified(String::from_utf8_lossy(&upstream).trim())
            .unwrap_or_else(|| RemoteBranch::origin_counterpart(trunk));
        let Some(out) = self.git_probe(&["merge-base", &remote.0, "HEAD"])? else {
            return Ok(None);
        };
        let merge_base = String::from_utf8_lossy(&out)
            .trim()
            .parse::<CommitOid>()
            .map_err(|error| GitError::Parse(format!("merge-base oid: {error}")))?;
        let local_tip = self.rev_parse_commit(&trunk_ref)?;
        if local_tip == merge_base
            || self
                .git_probe(&[
                    "merge-base",
                    "--is-ancestor",
                    &local_tip.to_string(),
                    &merge_base.to_string(),
                ])?
                .is_none()
        {
            return Ok(None);
        }
        Ok(Some(merge_base))
    }

    fn current_branch(&self) -> Result<Option<LocalBranch>, GitError> {
        Ok(self
            .git_probe(&["symbolic-ref", "--quiet", "HEAD"])?
            .and_then(|out| LocalBranch::from_qualified(String::from_utf8_lossy(&out).trim())))
    }

    fn local_branches(&self) -> Result<Vec<LocalBranch>, GitError> {
        let out = self.git(&["for-each-ref", "--format=%(refname)", "refs/heads/"], &[])?;
        let mut branches: Vec<LocalBranch> = String::from_utf8_lossy(&out)
            .lines()
            .filter_map(|line| LocalBranch::from_qualified(line.trim()))
            .collect();
        branches.sort();
        Ok(branches)
    }

    /// Revisions offered by the review header's selectors. Named refs come
    /// from git's ref database and recent commits come from reachable
    /// history, so remote clients see the daemon repository's actual state.
    pub fn ref_candidates(&self) -> Result<Vec<RefCandidate>, GitError> {
        let mut refs = self
            .local_branches()?
            .into_iter()
            .map(|branch| RefCandidate {
                ref_spec: branch.into_ref_spec(),
                subject: None,
            })
            .collect::<Vec<_>>();

        let tags = self.git(&["for-each-ref", "--format=%(refname)", "refs/tags/"], &[])?;
        refs.extend(
            String::from_utf8_lossy(&tags)
                .lines()
                .filter_map(|line| line.trim().strip_prefix("refs/tags/"))
                .filter(|name| !name.is_empty())
                .map(|name| RefCandidate {
                    ref_spec: RefSpec::Tag {
                        name: name.to_owned(),
                    },
                    subject: None,
                }),
        );

        let log = self.git(
            &["log", "--all", "--max-count=50", "--format=%H%x00%s"],
            &[],
        )?;
        for line in String::from_utf8_lossy(&log).lines() {
            let Some((oid, subject)) = line.split_once('\0') else {
                return Err(GitError::Parse("git log record has no separator".into()));
            };
            let oid = oid
                .parse::<CommitOid>()
                .map_err(|e| GitError::Parse(format!("invalid commit oid {oid:?}: {e}")))?;
            refs.push(RefCandidate {
                ref_spec: RefSpec::Commit { oid },
                subject: Some(subject.to_owned()),
            });
        }
        refs.push(RefCandidate {
            ref_spec: RefSpec::WorkingTree,
            subject: None,
        });
        Ok(refs)
    }

    fn reflog_parent(
        &self,
        current: &LocalBranch,
        branches: &[LocalBranch],
    ) -> Result<Option<LocalBranch>, GitError> {
        let reference = current.qualified();
        let Some(out) = self.git_probe(&["reflog", "show", "--format=%gs", &reference])? else {
            return Ok(None);
        };
        let source = String::from_utf8_lossy(&out).lines().find_map(|line| {
            line.strip_prefix("branch: Created from ")
                .filter(|source| *source != "HEAD")
                .map(str::to_owned)
        });
        let Some(source) = source else {
            return Ok(None);
        };
        // A local branch may itself be called `origin/parent`. Prefer the
        // literal local name before interpreting a remote shorthand.
        if let Some(branch) = branches
            .iter()
            .find(|branch| branch.as_str() == source && *branch != current)
        {
            return Ok(Some(branch.clone()));
        }
        let source = source
            .strip_prefix("refs/heads/")
            .or_else(|| source.strip_prefix("refs/remotes/origin/"))
            .or_else(|| source.strip_prefix("origin/"))
            .unwrap_or(&source);
        Ok(branches
            .iter()
            .find(|branch| branch.as_str() == source && *branch != current)
            .cloned())
    }

    fn closest_branch(
        &self,
        current: Option<&LocalBranch>,
        trunk: Option<&LocalBranch>,
        branches: &[LocalBranch],
    ) -> Result<Option<LocalBranch>, GitError> {
        let head = match self.rev_parse_commit("HEAD") {
            Ok(head) => head,
            Err(GitError::Resolve { .. }) => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut ranked = Vec::new();
        for branch in branches {
            if current == Some(branch) {
                continue;
            }
            let local_tip = self.rev_parse_commit(&branch.qualified())?;
            // Rank the effective trunk base alongside local parent branches.
            // Correcting trunk only after ranking would let a sibling from
            // the newer remote trunk beat the stale local trunk first.
            let branch_tip = if trunk == Some(branch) {
                self.advanced_remote_trunk_base(branch)?
                    .unwrap_or(local_tip)
            } else {
                local_tip
            };
            let branch_ref = branch_tip.to_string();
            // A descendant of HEAD is not an ancestor candidate. A second
            // branch at the exact same commit is retained: that is common
            // immediately after cutting a stacked branch.
            if branch_tip != head
                && self
                    .git_probe(&["merge-base", "--is-ancestor", "HEAD", &branch_ref])?
                    .is_some()
            {
                continue;
            }
            let Some(merge_base) = self.git_probe(&["merge-base", &branch_ref, "HEAD"])? else {
                continue;
            };
            let merge_base = String::from_utf8_lossy(&merge_base).trim().to_owned();
            let range = format!("{merge_base}..HEAD");
            let Some(count) = self.git_probe(&["rev-list", "--count", &range])? else {
                continue;
            };
            let count = String::from_utf8_lossy(&count)
                .trim()
                .parse::<u64>()
                .map_err(|error| GitError::Parse(format!("rev-list count: {error}")))?;
            let points_at_merge_base = branch_tip.to_string() == merge_base;
            let is_trunk = trunk == Some(branch);
            ranked.push((count, !points_at_merge_base, !is_trunk, branch.clone()));
        }
        ranked.sort();
        Ok(ranked.into_iter().next().map(|(_, _, _, branch)| branch))
    }

    fn trunk_branch(&self, branches: &[LocalBranch]) -> Result<Option<LocalBranch>, GitError> {
        let find = |name: &str| {
            branches
                .iter()
                .find(|branch| branch.as_str() == name)
                .cloned()
        };
        if let Some(out) =
            self.git_probe(&["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"])?
        {
            let target = String::from_utf8_lossy(&out);
            if let Some(branch) = target
                .trim()
                .strip_prefix("refs/remotes/origin/")
                .and_then(&find)
            {
                return Ok(Some(branch));
            }
        }
        if let Some(out) = self.git_probe(&["config", "--get", "init.defaultBranch"])?
            && let Some(branch) = find(String::from_utf8_lossy(&out).trim())
        {
            return Ok(Some(branch));
        }
        for name in ["main", "master", "trunk", "develop"] {
            if let Some(branch) = find(name) {
                return Ok(Some(branch));
            }
        }
        Ok((branches.len() == 1).then(|| branches[0].clone()))
    }

    /// Resolve a [`RefSpec`] to concrete content. `WorkingTree` snapshots the
    /// working tree as a new tree object.
    pub fn resolve(&self, spec: &RefSpec) -> Result<ResolvedRef, GitError> {
        let rev = match spec {
            RefSpec::Branch { name } => format!("refs/heads/{name}"),
            RefSpec::Tag { name } => format!("refs/tags/{name}"),
            RefSpec::Commit { oid } => oid.to_string(),
            RefSpec::Head => "HEAD".to_owned(),
            RefSpec::Upstream => "@{upstream}".to_owned(),
            RefSpec::WorkingTree => return self.working_tree(),
        };
        let commit = self.rev_parse_commit(&rev)?;
        let tree = self.commit_tree(commit)?;
        Ok(ResolvedRef {
            tree,
            source: ResolvedSource::Commit { oid: commit },
        })
    }

    /// `git rev-parse` of `<rev>^{commit}`.
    pub fn rev_parse_commit(&self, rev: &str) -> Result<CommitOid, GitError> {
        let spec = format!("{rev}^{{commit}}");
        let out = self
            .git(
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    "--end-of-options",
                    &spec,
                ],
                &[],
            )
            .map_err(|e| GitError::Resolve {
                rev: rev.to_owned(),
                reason: match e {
                    GitError::Command { stderr, .. } if stderr.is_empty() => {
                        "no such revision".into()
                    }
                    other @ (GitError::Open { .. }
                    | GitError::Command { .. }
                    | GitError::Resolve { .. }
                    | GitError::DefaultBase { .. }
                    | GitError::Object { .. }
                    | GitError::Parse(_)
                    | GitError::Io(_)) => other.to_string(),
                },
            })?;
        let text = String::from_utf8_lossy(&out);
        text.trim()
            .parse::<Oid>()
            .map(CommitOid::new)
            .map_err(|e| GitError::Parse(format!("rev-parse output {text:?}: {e}")))
    }

    fn commit_tree(&self, commit: CommitOid) -> Result<TreeOid, GitError> {
        let repo = self.local();
        let c = repo
            .find_commit(gix_id(commit.oid()))
            .map_err(|e| GitError::Object {
                oid: commit.oid(),
                reason: e.to_string(),
            })?;
        let tree = c.tree_id().map_err(|e| GitError::Object {
            oid: commit.oid(),
            reason: e.to_string(),
        })?;
        oid_from_gix(tree.detach())
            .map(TreeOid::new)
            .ok_or_else(|| GitError::Parse("non-SHA1 object id".into()))
    }

    // ---- objects ----------------------------------------------------------

    pub fn blob(&self, oid: BlobOid) -> Result<Vec<u8>, GitError> {
        let repo = self.local();
        let mut obj = repo
            .find_blob(gix_id(oid.oid()))
            .map_err(|e| GitError::Object {
                oid: oid.oid(),
                reason: e.to_string(),
            })?;
        Ok(obj.take_data())
    }

    /// Write `bytes` as a blob object and return its id.
    pub fn hash_blob(&self, bytes: &[u8]) -> Result<BlobOid, GitError> {
        let repo = self.local();
        let id = repo.write_blob(bytes).map_err(|e| GitError::Object {
            oid: Oid::zero(),
            reason: e.to_string(),
        })?;
        oid_from_gix(id.detach())
            .map(BlobOid::new)
            .ok_or_else(|| GitError::Parse("non-SHA1 object id".into()))
    }

    fn blob_size(repo: &gix::Repository, id: gix::ObjectId) -> Result<u64, GitError> {
        let header = repo.find_header(id).map_err(|e| GitError::Object {
            oid: oid_from_gix(id).unwrap_or(Oid::zero()),
            reason: e.to_string(),
        })?;
        Ok(header.size())
    }

    pub fn commit_info(&self, oid: CommitOid) -> Result<CommitInfo, GitError> {
        let repo = self.local();
        let err = |reason: String| GitError::Object {
            oid: oid.oid(),
            reason,
        };
        let c = repo
            .find_commit(gix_id(oid.oid()))
            .map_err(|e| err(e.to_string()))?;
        let tree = c.tree_id().map_err(|e| err(e.to_string()))?;
        let parents = c
            .parent_ids()
            .map(|p| oid_from_gix(p.detach()).map(CommitOid::new))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| err("non-SHA1 parent".into()))?;
        let author = sig(&c.author().map_err(|e| err(e.to_string()))?)?;
        let committer = sig(&c.committer().map_err(|e| err(e.to_string()))?)?;
        let message = c.message().map_err(|e| err(e.to_string()))?;
        let subject = message.summary().to_str_lossy().into_owned();
        let body = message
            .body()
            .map(|b| b.to_str_lossy().trim().to_owned())
            .unwrap_or_default();
        Ok(CommitInfo {
            oid,
            parents,
            tree: oid_from_gix(tree.detach())
                .map(TreeOid::new)
                .ok_or_else(|| err("non-SHA1 tree".into()))?,
            author,
            committer,
            subject,
            body,
        })
    }

    /// Commits reachable from `head` but not `base`, newest first
    /// (`git rev-list --topo-order base..head`).
    pub fn commits_between(
        &self,
        base: CommitOid,
        head: CommitOid,
    ) -> Result<Vec<CommitInfo>, GitError> {
        let range = format!("{base}..{head}");
        let out = self.git(&["rev-list", "--topo-order", &range], &[])?;
        String::from_utf8_lossy(&out)
            .lines()
            .map(|l| {
                let oid: Oid = l
                    .trim()
                    .parse()
                    .map_err(|e| GitError::Parse(format!("rev-list line {l:?}: {e}")))?;
                self.commit_info(CommitOid::new(oid))
            })
            .collect()
    }

    // ---- trees ------------------------------------------------------------

    /// Full recursive listing, sorted by path.
    pub fn tree_snapshot(&self, repo_id: RepoId, root: TreeOid) -> Result<TreeSnapshot, GitError> {
        let repo = self.local();
        let err = |reason: String| GitError::Object {
            oid: root.oid(),
            reason,
        };
        let tree = repo
            .find_tree(gix_id(root.oid()))
            .map_err(|e| err(e.to_string()))?;
        let mut entries = Vec::new();
        let recorded = tree
            .traverse()
            .breadthfirst
            .files()
            .map_err(|e| err(e.to_string()))?;
        for rec in recorded {
            let Some(path) = RepoPath::new(rec.filepath.to_str_lossy().into_owned()).ok() else {
                continue;
            };
            let Some(oid) = oid_from_gix(rec.oid) else {
                return Err(err("non-SHA1 entry".into()));
            };
            let kind = match rec.mode.kind() {
                K::Tree => TreeEntryKind::Dir {
                    oid: TreeOid::new(oid),
                },
                K::Blob | K::BlobExecutable => TreeEntryKind::File {
                    oid: BlobOid::new(oid),
                    size: Self::blob_size(&repo, rec.oid)?,
                    executable: matches!(rec.mode.kind(), K::BlobExecutable),
                },
                K::Link => TreeEntryKind::Symlink {
                    oid: BlobOid::new(oid),
                },
                K::Commit => TreeEntryKind::Submodule {
                    commit: CommitOid::new(oid),
                },
            };
            entries.push(TreeEntry { path, kind });
        }
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(TreeSnapshot {
            repo_id,
            root_oid: root,
            entries,
        })
    }

    /// Entries added/removed/changed between two trees (no rename pairing).
    pub fn tree_delta(
        &self,
        repo_id: RepoId,
        from: TreeOid,
        to: TreeOid,
    ) -> Result<TreeDelta, GitError> {
        let mut delta = TreeDelta {
            repo_id,
            from_root: from,
            to_root: to,
            added: vec![],
            removed: vec![],
            changed: vec![],
        };
        let repo = self.local();
        for raw in self.raw_diff(from, to, false)? {
            match raw.status {
                RawStatus::Added => {
                    delta
                        .added
                        .push(Self::entry_for(&repo, &raw.path, raw.new_mode, raw.new)?);
                }
                RawStatus::Deleted => delta.removed.push(raw.path),
                RawStatus::Modified | RawStatus::TypeChanged => {
                    delta
                        .changed
                        .push(Self::entry_for(&repo, &raw.path, raw.new_mode, raw.new)?);
                }
                RawStatus::Renamed { from } => {
                    delta.removed.push(from);
                    delta
                        .added
                        .push(Self::entry_for(&repo, &raw.path, raw.new_mode, raw.new)?);
                }
            }
        }
        Ok(delta)
    }

    fn entry_for(
        repo: &gix::Repository,
        path: &RepoPath,
        mode: u32,
        oid: Oid,
    ) -> Result<TreeEntry, GitError> {
        let kind = match mode & 0o170_000 {
            0o040_000 => TreeEntryKind::Dir {
                oid: TreeOid::new(oid),
            },
            0o120_000 => TreeEntryKind::Symlink {
                oid: BlobOid::new(oid),
            },
            0o160_000 => TreeEntryKind::Submodule {
                commit: CommitOid::new(oid),
            },
            _ => TreeEntryKind::File {
                oid: BlobOid::new(oid),
                size: Self::blob_size(repo, gix_id(oid))?,
                executable: mode & 0o111 != 0,
            },
        };
        Ok(TreeEntry {
            path: path.clone(),
            kind,
        })
    }

    /// Changed files between two trees with rename detection. Submodule
    /// entries are skipped (they are not blobs and cannot be rendered).
    pub fn changed_files(
        &self,
        base: TreeOid,
        head: TreeOid,
    ) -> Result<Vec<FileChangeRaw>, GitError> {
        let mut out = Vec::new();
        for raw in self.raw_diff(base, head, true)? {
            if raw.old_mode & 0o170_000 == 0o160_000 || raw.new_mode & 0o170_000 == 0o160_000 {
                continue;
            }
            let kind = match raw.status {
                RawStatus::Added => ChangeKind::Added {
                    new: BlobOid::new(raw.new),
                },
                RawStatus::Deleted => ChangeKind::Deleted {
                    old: BlobOid::new(raw.old),
                },
                RawStatus::Modified | RawStatus::TypeChanged => ChangeKind::Modified {
                    old: BlobOid::new(raw.old),
                    new: BlobOid::new(raw.new),
                },
                RawStatus::Renamed { from } => ChangeKind::Renamed {
                    from,
                    old: BlobOid::new(raw.old),
                    new: BlobOid::new(raw.new),
                },
            };
            out.push(FileChangeRaw {
                path: raw.path,
                kind,
            });
        }
        Ok(out)
    }

    fn raw_diff(
        &self,
        from: TreeOid,
        to: TreeOid,
        renames: bool,
    ) -> Result<Vec<RawChange>, GitError> {
        let (f, t) = (from.to_string(), to.to_string());
        let mut args = vec!["diff-tree", "-r", "-z", "--raw", "--no-color"];
        if renames {
            args.push("-M");
        }
        args.push(&f);
        args.push(&t);
        let out = self.git(&args, &[])?;
        parse_raw_diff(&out)
    }

    /// Keep immutable content and commit provenance reachable through git GC.
    /// Nits owns these namespaced refs; moving a branch never changes them.
    pub fn retain_revision(
        &self,
        review: nits_protocol::ReviewId,
        revision: &ResolvedRef,
    ) -> Result<(), GitError> {
        self.git(
            &[
                "update-ref",
                &format!("refs/nits/reviews/{review}/trees/{}", revision.tree),
                &revision.tree.to_string(),
            ],
            &[],
        )?;
        let commit = match revision.source {
            ResolvedSource::Commit { oid } => Some(oid),
            ResolvedSource::WorkingTree { head, .. } => head,
        };
        if let Some(oid) = commit {
            self.git(
                &[
                    "update-ref",
                    &format!("refs/nits/reviews/{review}/commits/{oid}"),
                    &oid.to_string(),
                ],
                &[],
            )?;
        }
        Ok(())
    }

    // ---- working tree -----------------------------------------------------

    /// Snapshot the working tree into a real tree object via a temporary
    /// index. Respects `.gitignore`; untracked files are included; deleted
    /// files are absent.
    pub fn working_tree(&self) -> Result<ResolvedRef, GitError> {
        let git_dir = self.local().git_dir().to_path_buf();
        // A private directory owns both the index and any Git lock file on
        // every return path. With no real index, leave the path absent so Git
        // creates a valid empty index instead of reading a zero-byte file.
        let tmp = tempfile::Builder::new()
            .prefix("nits-index-")
            .tempdir_in(&git_dir)?;
        let tmp_path = tmp.path().join("index");
        // Seed from the real index so stat caching makes `add -A` incremental.
        let real_index = git_dir.join("index");
        if real_index.exists() {
            // Git uses the index mtime to detect racily clean entries. A fresh
            // copy timestamp could hide same-size edits from an earlier second.
            // Read metadata and bytes from one handle so atomic index replacement
            // cannot pair another generation's timestamp with these entries.
            let mut source = std::fs::File::open(&real_index)?;
            let modified = source.metadata()?.modified()?;
            let mut index = std::fs::File::create(&tmp_path)?;
            std::io::copy(&mut source, &mut index)?;
            index.set_modified(modified)?;
        }
        let env: &[(&str, &Path)] = &[("GIT_INDEX_FILE", tmp_path.as_path())];
        self.git(&["add", "-A", "--ignore-errors", "--", "."], env)?;
        let out = self.git(&["write-tree"], env)?;
        let text = String::from_utf8_lossy(&out);
        let tree: Oid = text
            .trim()
            .parse()
            .map_err(|e| GitError::Parse(format!("write-tree output {text:?}: {e}")))?;
        let tree = TreeOid::new(tree);

        let branch = self
            .git(&["symbolic-ref", "--short", "--quiet", "HEAD"], &[])
            .ok()
            .and_then(|out| {
                let name = String::from_utf8_lossy(&out).trim().to_owned();
                (!name.is_empty()).then_some(name)
            });
        let (dirty, head) = match self.rev_parse_commit("HEAD") {
            Ok(head) => {
                let head_tree = self.commit_tree(head)?;
                let mut paths: Vec<RepoPath> = Vec::new();
                for raw in self.raw_diff(head_tree, tree, false)? {
                    paths.push(raw.path);
                }
                paths.sort();
                (paths, Some(head))
            }
            // Unborn branch: everything is new.
            Err(GitError::Resolve { .. }) => {
                let snap = self.tree_snapshot(RepoId::nil(), tree)?;
                let paths = snap
                    .entries
                    .into_iter()
                    .filter(|e| !matches!(e.kind, TreeEntryKind::Dir { .. }))
                    .map(|e| e.path)
                    .collect();
                (paths, None)
            }
            Err(e) => return Err(e),
        };
        Ok(ResolvedRef {
            tree,
            source: ResolvedSource::WorkingTree {
                dirty,
                branch,
                head,
            },
        })
    }
}

/// A changed file without its `repo_id` (the review layer attaches that).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChangeRaw {
    pub path: RepoPath,
    pub kind: ChangeKind,
}

fn sig(s: &gix::actor::SignatureRef<'_>) -> Result<Sig, GitError> {
    let time = s
        .time()
        .map_err(|e| GitError::Parse(format!("signature time: {e}")))?;
    Ok(Sig {
        name: s.name.to_str_lossy().into_owned(),
        email: s.email.to_str_lossy().into_owned(),
        time: Timestamp::from_millis(time.seconds * 1000),
        offset_minutes: time.offset / 60,
    })
}

#[derive(Debug)]
enum RawStatus {
    Added,
    Deleted,
    Modified,
    TypeChanged,
    Renamed { from: RepoPath },
}

#[derive(Debug)]
struct RawChange {
    old_mode: u32,
    new_mode: u32,
    old: Oid,
    new: Oid,
    status: RawStatus,
    path: RepoPath,
}

/// Parse `git diff-tree -r -z --raw` output.
fn parse_raw_diff(out: &[u8]) -> Result<Vec<RawChange>, GitError> {
    let mut fields = out.split(|b| *b == 0).filter(|f| !f.is_empty());
    let mut changes = Vec::new();
    while let Some(meta) = fields.next() {
        let meta = std::str::from_utf8(meta).map_err(|e| GitError::Parse(e.to_string()))?;
        let meta = meta
            .strip_prefix(':')
            .ok_or_else(|| GitError::Parse(format!("raw line {meta:?}")))?;
        let parts: Vec<&str> = meta.split(' ').collect();
        let [old_mode, new_mode, old, new, status] = parts[..] else {
            return Err(GitError::Parse(format!("raw line {meta:?}")));
        };
        let parse_mode =
            |m: &str| u32::from_str_radix(m, 8).map_err(|e| GitError::Parse(e.to_string()));
        let parse_oid = |o: &str| o.parse::<Oid>().map_err(|e| GitError::Parse(e.to_string()));
        let next_path = |fields: &mut dyn Iterator<Item = &[u8]>| -> Result<RepoPath, GitError> {
            let p = fields
                .next()
                .ok_or_else(|| GitError::Parse("missing path".into()))?;
            RepoPath::new(String::from_utf8_lossy(p).into_owned())
                .map_err(|e| GitError::Parse(e.to_string()))
        };
        let (status, path) = match status.as_bytes()[0] {
            b'A' => (RawStatus::Added, next_path(&mut fields)?),
            b'D' => (RawStatus::Deleted, next_path(&mut fields)?),
            b'M' => (RawStatus::Modified, next_path(&mut fields)?),
            b'T' => (RawStatus::TypeChanged, next_path(&mut fields)?),
            b'R' | b'C' => {
                let from = next_path(&mut fields)?;
                let to = next_path(&mut fields)?;
                (RawStatus::Renamed { from }, to)
            }
            other => {
                return Err(GitError::Parse(format!("unknown status {}", other as char)));
            }
        };
        changes.push(RawChange {
            old_mode: parse_mode(old_mode)?,
            new_mode: parse_mode(new_mode)?,
            old: parse_oid(old)?,
            new: parse_oid(new)?,
            status,
            path,
        });
    }
    Ok(changes)
}
