//! Git engine tests over real repositories.

use nits_protocol::{
    ChangeKind, CommitOid, RefCandidate, RefSpec, RepoId, ResolvedSource, TreeEntryKind, TreeOid,
};
use nits_review_core::git::{Repo, is_binary};
use nits_test_support::{RepoBuilder, TestRepo, files};

fn commit(s: &str) -> CommitOid {
    CommitOid::new(s.parse().unwrap())
}

#[test]
fn resolves_every_refspec_variant() {
    let t = RepoBuilder::new()
        .commit("one", files!["a.txt" => "a\n"])
        .tag("v1")
        .branch("feature")
        .commit("two", files!["a.txt" => "b\n"])
        .build()
        .unwrap();
    // an upstream for `feature`: point it at main
    t.git(&["branch", "--set-upstream-to=main", "feature"])
        .unwrap();
    let repo = Repo::open(t.path()).unwrap();
    let main = commit(&t.rev_parse("main").unwrap());
    let feature = commit(&t.rev_parse("feature").unwrap());

    let r = repo
        .resolve(&RefSpec::Branch {
            name: "main".into(),
        })
        .unwrap();
    assert_eq!(r.source, ResolvedSource::Commit { oid: main });
    let r = repo.resolve(&RefSpec::Tag { name: "v1".into() }).unwrap();
    assert_eq!(r.source, ResolvedSource::Commit { oid: main });
    let r = repo.resolve(&RefSpec::Head).unwrap();
    assert_eq!(r.source, ResolvedSource::Commit { oid: feature });
    let r = repo.resolve(&RefSpec::Upstream).unwrap();
    assert_eq!(r.source, ResolvedSource::Commit { oid: main });
    let r = repo.resolve(&RefSpec::Commit { oid: feature }).unwrap();
    assert_eq!(
        r.tree.to_string(),
        t.git(&["rev-parse", "feature^{tree}"]).unwrap()
    );

    let err = repo
        .resolve(&RefSpec::Branch {
            name: "nope".into(),
        })
        .unwrap_err();
    assert!(err.to_string().contains("nope"), "{err}");
}

#[test]
fn ref_candidates_come_from_real_branches_tags_commits_and_working_tree() {
    let t = RepoBuilder::new()
        .commit("base subject", files!["a.txt" => "a\n"])
        .tag("v1")
        .branch("feature")
        .commit("selector subject", files!["a.txt" => "b\n"])
        .build()
        .unwrap();
    let feature = commit(&t.rev_parse("feature").unwrap());
    let candidates = Repo::open(t.path()).unwrap().ref_candidates().unwrap();

    assert!(candidates.contains(&RefCandidate {
        ref_spec: RefSpec::Branch {
            name: "feature".into()
        },
        subject: None,
    }));
    assert!(candidates.contains(&RefCandidate {
        ref_spec: RefSpec::Tag { name: "v1".into() },
        subject: None,
    }));
    assert!(candidates.contains(&RefCandidate {
        ref_spec: RefSpec::Commit { oid: feature },
        subject: Some("selector subject".into()),
    }));
    assert_eq!(
        candidates.last(),
        Some(&RefCandidate {
            ref_spec: RefSpec::WorkingTree,
            subject: None,
        })
    );
}

#[test]
fn default_base_finds_master_without_a_remote() {
    let t = RepoBuilder::new()
        .commit("base", files!["a.txt" => "base\n"])
        .build()
        .unwrap();
    t.git(&["branch", "-m", "master"]).unwrap();
    t.git(&["checkout", "-q", "-b", "feature"]).unwrap();
    t.git(&["commit", "-q", "--allow-empty", "-m", "feature"])
        .unwrap();

    assert_eq!(
        Repo::open(t.path()).unwrap().default_base().unwrap(),
        RefSpec::Branch {
            name: "master".into()
        }
    );
}

#[test]
fn default_base_finds_a_nonstandard_trunk() {
    let t = RepoBuilder::new()
        .commit("base", files!["a.txt" => "base\n"])
        .build()
        .unwrap();
    t.git(&["branch", "-m", "integration"]).unwrap();
    t.git(&["config", "init.defaultBranch", "integration"])
        .unwrap();
    t.git(&["checkout", "-q", "-b", "topic"]).unwrap();
    t.git(&["commit", "-q", "--allow-empty", "-m", "topic"])
        .unwrap();

    assert_eq!(
        Repo::open(t.path()).unwrap().default_base().unwrap(),
        RefSpec::Branch {
            name: "integration".into()
        }
    );
}

/// Keep local main at A, origin/main at B (which deletes an unrelated file),
/// and a feature created explicitly from main. No network remote is needed.
fn stale_main_repo() -> TestRepo {
    let t = RepoBuilder::new()
        .commit("base", files!["old-frontend.txt" => "obsolete\n"])
        .branch("remote-main")
        .commit_removing("remove old frontend", &["old-frontend.txt"])
        .checkout("main")
        .build()
        .unwrap();
    t.git(&[
        "update-ref",
        "refs/remotes/origin/main",
        "refs/heads/remote-main",
    ])
    .unwrap();
    t.git(&["branch", "-D", "remote-main"]).unwrap();
    t.git(&["checkout", "-q", "-b", "feature", "main"]).unwrap();
    t.write_file("feature.txt", b"feature\n").unwrap();
    t.git(&["add", "feature.txt"]).unwrap();
    t.git(&["commit", "-q", "-m", "feature"]).unwrap();
    t
}

fn assert_feature_only_default_base(t: &TestRepo, expected: CommitOid) {
    let refs_before = t.git(&["show-ref"]).unwrap();
    let head_before = t.rev_parse("HEAD").unwrap();
    let repo = Repo::open(t.path()).unwrap();
    let base_spec = repo.default_base().unwrap();
    assert_eq!(base_spec, RefSpec::Commit { oid: expected });
    let base = repo.resolve(&base_spec).unwrap();
    let head = repo.resolve(&RefSpec::Head).unwrap();
    let paths: Vec<_> = repo
        .changed_files(base.tree, head.tree)
        .unwrap()
        .into_iter()
        .map(|change| change.path.to_string())
        .collect();
    assert_eq!(paths, vec!["feature.txt"]);
    assert_eq!(t.git(&["show-ref"]).unwrap(), refs_before);
    assert_eq!(t.rev_parse("HEAD").unwrap(), head_before);
    assert_eq!(t.git(&["status", "--porcelain"]).unwrap(), "");
}

#[test]
fn default_base_excludes_remote_trunk_changes_after_a_rebase_with_stale_main() {
    let t = stale_main_repo();
    t.git(&["rebase", "refs/remotes/origin/main"]).unwrap();
    let remote = commit(&t.rev_parse("refs/remotes/origin/main").unwrap());

    // The named reflog source `main` still points to A after the rebase.
    assert_feature_only_default_base(&t, remote);

    // Repositories with expired reflogs go through local ancestor ranking.
    t.git(&["reflog", "expire", "--expire=all", "--all"])
        .unwrap();
    assert_feature_only_default_base(&t, remote);
}

fn stale_main_with_unrelated_sibling() -> TestRepo {
    let t = stale_main_repo();
    t.git(&["rebase", "refs/remotes/origin/main"]).unwrap();
    t.git(&[
        "checkout",
        "-q",
        "-b",
        "unrelated-sibling",
        "refs/remotes/origin/main",
    ])
    .unwrap();
    t.write_file("sibling.txt", b"unrelated sibling work\n")
        .unwrap();
    t.git(&["add", "sibling.txt"]).unwrap();
    t.git(&["commit", "-q", "-m", "unrelated sibling"]).unwrap();
    t.git(&["checkout", "-q", "feature"]).unwrap();
    t
}

#[test]
fn default_base_ranks_remote_trunk_before_sibling_without_named_reflog_source() {
    let t = stale_main_with_unrelated_sibling();
    let remote = commit(&t.rev_parse("refs/remotes/origin/main").unwrap());
    // Recreate the feature at its tip with only `HEAD` as its creation source.
    t.git(&["branch", "-m", "original-feature"]).unwrap();
    t.git(&["checkout", "-q", "-b", "feature"]).unwrap();
    t.git(&["branch", "-D", "original-feature"]).unwrap();
    assert_eq!(
        t.git(&["reflog", "show", "--format=%gs", "refs/heads/feature"])
            .unwrap(),
        "branch: Created from HEAD"
    );

    assert_feature_only_default_base(&t, remote);
}

#[test]
fn default_base_ranks_remote_trunk_before_sibling_with_expired_reflog() {
    let t = stale_main_with_unrelated_sibling();
    let remote = commit(&t.rev_parse("refs/remotes/origin/main").unwrap());
    t.git(&["reflog", "expire", "--expire=all", "--all"])
        .unwrap();
    assert_eq!(
        t.git(&["reflog", "show", "refs/heads/feature"]).unwrap(),
        ""
    );

    assert_feature_only_default_base(&t, remote);
}

#[test]
fn default_base_in_linked_worktree_uses_remote_trunk_shared_with_stale_main() {
    let t = stale_main_repo();
    t.git(&["rebase", "refs/remotes/origin/main"]).unwrap();
    let remote = commit(&t.rev_parse("refs/remotes/origin/main").unwrap());
    t.git(&["checkout", "-q", "main"]).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("feature");
    t.git(&["worktree", "add", "-q", path.to_str().unwrap(), "feature"])
        .unwrap();
    let refs_before = t.git(&["show-ref"]).unwrap();

    let repo = Repo::open(&path).unwrap();
    let base_spec = repo.default_base().unwrap();
    assert_eq!(base_spec, RefSpec::Commit { oid: remote });
    let base = repo.resolve(&base_spec).unwrap();
    let head = repo.resolve(&RefSpec::WorkingTree).unwrap();
    let paths: Vec<_> = repo
        .changed_files(base.tree, head.tree)
        .unwrap()
        .into_iter()
        .map(|change| change.path.to_string())
        .collect();
    assert_eq!(paths, vec!["feature.txt"]);
    assert_eq!(t.git(&["show-ref"]).unwrap(), refs_before);
}

#[test]
fn default_base_uses_remote_merge_base_when_remote_has_advanced_again() {
    let t = stale_main_repo();
    t.git(&["rebase", "refs/remotes/origin/main"]).unwrap();
    let rebased_onto = commit(&t.rev_parse("refs/remotes/origin/main").unwrap());
    t.git(&["checkout", "-q", "--detach", "refs/remotes/origin/main"])
        .unwrap();
    t.write_file("later-trunk.txt", b"not in the feature\n")
        .unwrap();
    t.git(&["add", "later-trunk.txt"]).unwrap();
    t.git(&["commit", "-q", "-m", "later trunk change"])
        .unwrap();
    t.git(&["update-ref", "refs/remotes/origin/main", "HEAD"])
        .unwrap();
    t.git(&["checkout", "-q", "feature"]).unwrap();

    assert_feature_only_default_base(&t, rebased_onto);
}

#[test]
fn default_base_keeps_local_trunk_when_feature_has_not_incorporated_remote() {
    let t = stale_main_repo();
    let repo = Repo::open(t.path()).unwrap();
    let base_spec = repo.default_base().unwrap();
    assert_eq!(
        base_spec,
        RefSpec::Branch {
            name: "main".into()
        }
    );
    let base = repo.resolve(&base_spec).unwrap();
    let head = repo.resolve(&RefSpec::Head).unwrap();
    let paths: Vec<_> = repo
        .changed_files(base.tree, head.tree)
        .unwrap()
        .into_iter()
        .map(|change| change.path.to_string())
        .collect();
    assert_eq!(paths, vec!["feature.txt"]);
}

#[test]
fn default_base_keeps_diverged_local_trunk() {
    let t = stale_main_repo();
    t.git(&["rebase", "refs/remotes/origin/main"]).unwrap();
    t.git(&["checkout", "-q", "main"]).unwrap();
    t.write_file("local-trunk.txt", b"local trunk work\n")
        .unwrap();
    t.git(&["add", "local-trunk.txt"]).unwrap();
    t.git(&["commit", "-q", "-m", "diverged local trunk"])
        .unwrap();
    t.git(&["checkout", "-q", "feature"]).unwrap();

    assert_eq!(
        Repo::open(t.path()).unwrap().default_base().unwrap(),
        RefSpec::Branch {
            name: "main".into()
        }
    );
}

#[test]
fn default_base_preserves_stack_parents_with_a_stale_local_trunk() {
    for parent in ["stack-parent", "origin/parent"] {
        let t = stale_main_repo();
        t.git(&["rebase", "refs/remotes/origin/main"]).unwrap();
        t.git(&["branch", "-m", parent]).unwrap();
        // `origin/parent` is a literal local branch, distinct from `parent`.
        t.git(&["branch", "parent", "refs/heads/main"]).unwrap();
        t.git(&["checkout", "-q", "-b", "child", parent]).unwrap();
        t.write_file("child.txt", b"child\n").unwrap();
        t.git(&["add", "child.txt"]).unwrap();
        t.git(&["commit", "-q", "-m", "child"]).unwrap();

        let repo = Repo::open(t.path()).unwrap();
        let expected = RefSpec::Branch {
            name: parent.into(),
        };
        assert_eq!(repo.default_base().unwrap(), expected);
        t.git(&["reflog", "expire", "--expire=all", "--all"])
            .unwrap();
        assert_eq!(repo.default_base().unwrap(), expected);
        let base = repo.resolve(&expected).unwrap();
        let head = repo.resolve(&RefSpec::Head).unwrap();
        let paths: Vec<_> = repo
            .changed_files(base.tree, head.tree)
            .unwrap()
            .into_iter()
            .map(|change| change.path.to_string())
            .collect();
        assert_eq!(paths, vec!["child.txt"]);
    }
}

#[test]
fn default_base_uses_configured_trunk_upstream_with_different_remote_and_branch_names() {
    let t = stale_main_repo();
    t.git(&["rebase", "refs/remotes/origin/main"]).unwrap();
    let remote = commit(&t.rev_parse("refs/remotes/origin/main").unwrap());
    t.git(&["branch", "-m", "main", "release/stable"]).unwrap();
    t.git(&["config", "init.defaultBranch", "release/stable"])
        .unwrap();
    t.git(&[
        "remote",
        "add",
        "upstream",
        "https://example.invalid/repo.git",
    ])
    .unwrap();
    t.git(&[
        "update-ref",
        "refs/remotes/upstream/releases/current",
        &remote.to_string(),
    ])
    .unwrap();
    t.git(&[
        "branch",
        "--set-upstream-to=upstream/releases/current",
        "release/stable",
    ])
    .unwrap();
    // A different origin tip must not override the configured upstream.
    t.git(&["update-ref", "refs/remotes/origin/release/stable", "HEAD"])
        .unwrap();

    assert_feature_only_default_base(&t, remote);
}

#[test]
fn default_base_remote_and_local_refs_are_unambiguous_with_same_named_tags() {
    let t = stale_main_repo();
    t.git(&["rebase", "refs/remotes/origin/main"]).unwrap();
    let remote = commit(&t.rev_parse("refs/remotes/origin/main").unwrap());
    t.git(&["tag", "main", "refs/heads/feature"]).unwrap();
    t.git(&["tag", "feature", "refs/heads/main"]).unwrap();
    t.git(&["tag", "origin/main", "refs/heads/main"]).unwrap();
    t.git(&[
        "symbolic-ref",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
    ])
    .unwrap();

    assert_feature_only_default_base(&t, remote);
}

#[test]
fn default_base_keeps_checked_out_trunk_even_when_remote_is_ahead_and_tag_is_ambiguous() {
    let t = stale_main_repo();
    t.git(&["checkout", "-q", "main"]).unwrap();
    t.git(&["tag", "main", "refs/heads/feature"]).unwrap();

    assert_eq!(
        Repo::open(t.path()).unwrap().default_base().unwrap(),
        RefSpec::Branch {
            name: "main".into()
        }
    );
}

#[test]
fn default_base_uses_the_parent_of_a_stacked_branch() {
    let t = RepoBuilder::new()
        .commit("base", files!["a.txt" => "base\n"])
        .branch("feature-1")
        .commit("one", files!["a.txt" => "one\n"])
        .branch("feature-2")
        .commit("two", files!["a.txt" => "two\n"])
        .build()
        .unwrap();

    assert_eq!(
        Repo::open(t.path()).unwrap().default_base().unwrap(),
        RefSpec::Branch {
            name: "feature-1".into()
        }
    );
}

#[test]
fn default_base_ranks_the_branch_when_a_tag_has_the_same_name() {
    let t = RepoBuilder::new()
        .commit("base", files!["a.txt" => "base\n"])
        .build()
        .unwrap();
    t.git(&["branch", "candidate", "main"]).unwrap();
    t.git(&["checkout", "-q", "-b", "parent"]).unwrap();
    t.git(&["commit", "-q", "--allow-empty", "-m", "parent"])
        .unwrap();
    t.git(&["tag", "candidate"]).unwrap();
    t.git(&["checkout", "-q", "-b", "child"]).unwrap();
    t.git(&["commit", "-q", "--allow-empty", "-m", "child"])
        .unwrap();

    assert_eq!(
        Repo::open(t.path()).unwrap().default_base().unwrap(),
        RefSpec::Branch {
            name: "parent".into()
        }
    );
}

#[test]
fn default_base_does_not_treat_a_child_branch_as_the_trunks_parent() {
    let t = RepoBuilder::new()
        .commit("base", files!["a.txt" => "base\n"])
        .branch("feature")
        .commit("feature", files!["a.txt" => "feature\n"])
        .checkout("main")
        .build()
        .unwrap();

    assert_eq!(
        Repo::open(t.path()).unwrap().default_base().unwrap(),
        RefSpec::Branch {
            name: "main".into()
        }
    );
}

#[test]
fn default_base_rejects_a_child_of_an_unrecognized_custom_trunk() {
    let t = RepoBuilder::new()
        .commit("base", files!["a.txt" => "base\n"])
        .build()
        .unwrap();
    t.git(&["branch", "-m", "production"]).unwrap();
    t.git(&["checkout", "-q", "-b", "feature"]).unwrap();
    t.git(&["commit", "-q", "--allow-empty", "-m", "feature"])
        .unwrap();
    t.git(&["checkout", "-q", "production"]).unwrap();

    let error = Repo::open(t.path()).unwrap().default_base().unwrap_err();
    assert!(
        error
            .to_string()
            .contains("cannot determine a default review base"),
        "{error}"
    );
}

#[test]
fn default_base_failure_names_the_repo_and_every_fallback() {
    let t = TestRepo::init().unwrap();
    let error = Repo::open(t.path()).unwrap().default_base().unwrap_err();
    let message = error.to_string();
    assert!(
        message.contains(&t.path().display().to_string()),
        "{message}"
    );
    assert!(message.contains("reflog"), "{message}");
    assert!(message.contains("closest ancestor"), "{message}");
    assert!(message.contains("origin/HEAD"), "{message}");
    assert!(message.contains("init.defaultBranch"), "{message}");
    assert!(message.contains("pass --base"), "{message}");
}

#[test]
fn changed_files_detects_add_delete_modify_rename() {
    let t = RepoBuilder::new()
        .commit(
            "one",
            files![
                "keep.txt" => "same\n",
                "mod.txt" => "old\n",
                "gone.txt" => "bye\n",
                "old_name.rs" => "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\n",
            ],
        )
        .branch("feature")
        .commit_removing("rm", &["gone.txt", "old_name.rs"])
        .commit(
            "two",
            files![
                "mod.txt" => "new\n",
                "added.txt" => "hi\n",
                "new_name.rs" => "fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\n",
            ],
        )
        .build()
        .unwrap();
    let repo = Repo::open(t.path()).unwrap();
    let base = repo
        .resolve(&RefSpec::Branch {
            name: "main".into(),
        })
        .unwrap();
    let head = repo
        .resolve(&RefSpec::Branch {
            name: "feature".into(),
        })
        .unwrap();
    let mut changes = repo.changed_files(base.tree, head.tree).unwrap();
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    let summary: Vec<(String, &str)> = changes
        .iter()
        .map(|c| {
            let k = match &c.kind {
                ChangeKind::Added { .. } => "A",
                ChangeKind::Deleted { .. } => "D",
                ChangeKind::Modified { .. } => "M",
                ChangeKind::Renamed { .. } => "R",
            };
            (c.path.to_string(), k)
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("added.txt".to_string(), "A"),
            ("gone.txt".to_string(), "D"),
            ("mod.txt".to_string(), "M"),
            ("new_name.rs".to_string(), "R"),
        ]
    );
    let ChangeKind::Renamed { from, old, new } = &changes[3].kind else {
        panic!("expected rename");
    };
    assert_eq!(from.as_str(), "old_name.rs");
    assert_eq!(old, new, "identical content keeps the same blob");
    assert_eq!(
        repo.blob(*new).unwrap(),
        b"fn a() {}\nfn b() {}\nfn c() {}\nfn d() {}\n"
    );
}

#[test]
fn tree_snapshot_is_flat_sorted_and_typed() {
    let t = RepoBuilder::new()
        .commit(
            "one",
            files!["src/lib.rs" => "x", "README.md" => "r", "src/bin/main.rs" => "m"],
        )
        .build()
        .unwrap();
    t.git(&["update-index", "--chmod=+x", "src/bin/main.rs"])
        .unwrap();
    t.git(&["commit", "-q", "-m", "exec"]).unwrap();
    let repo = Repo::open(t.path()).unwrap();
    let head = repo.resolve(&RefSpec::Head).unwrap();
    let snap = repo.tree_snapshot(RepoId::nil(), head.tree).unwrap();
    let paths: Vec<&str> = snap.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(
        paths,
        vec![
            "README.md",
            "src",
            "src/bin",
            "src/bin/main.rs",
            "src/lib.rs"
        ]
    );
    assert!(matches!(snap.entries[1].kind, TreeEntryKind::Dir { .. }));
    assert!(matches!(
        snap.entries[3].kind,
        TreeEntryKind::File {
            executable: true,
            size: 1,
            ..
        }
    ));
    assert!(matches!(
        snap.entries[4].kind,
        TreeEntryKind::File {
            executable: false,
            size: 1,
            ..
        }
    ));
}

#[test]
fn working_tree_snapshot_reflects_unstaged_edits_untracked_and_deletes() {
    let t = RepoBuilder::new()
        .commit("one", files!["a.txt" => "a\n", "b.txt" => "b\n", "c.txt" => "c\n", ".gitignore" => "ignored.txt\n"])
        .write(files!["a.txt" => "changed\n", "new.txt" => "n\n", "ignored.txt" => "i\n"])
        .remove(&["c.txt"])
        .build()
        .unwrap();
    let repo = Repo::open(t.path()).unwrap();
    let wt = repo.resolve(&RefSpec::WorkingTree).unwrap();
    let ResolvedSource::WorkingTree { dirty, .. } = &wt.source else {
        panic!("expected working tree");
    };
    let dirty: Vec<&str> = dirty.iter().map(nits_protocol::RepoPath::as_str).collect();
    assert_eq!(dirty, vec!["a.txt", "c.txt", "new.txt"]);

    let snap = repo.tree_snapshot(RepoId::nil(), wt.tree).unwrap();
    let paths: Vec<&str> = snap.entries.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, vec![".gitignore", "a.txt", "b.txt", "new.txt"]);
    let TreeEntryKind::File { oid, .. } = snap.entries[1].kind else {
        panic!()
    };
    assert_eq!(repo.blob(oid).unwrap(), b"changed\n");

    // The real index was not touched.
    assert_eq!(t.git(&["diff", "--cached", "--name-only"]).unwrap(), "");
    // Snapshot is stable when nothing changed, and changes when content does.
    assert_eq!(repo.resolve(&RefSpec::WorkingTree).unwrap().tree, wt.tree);
    t.write_file("a.txt", b"again\n").unwrap();
    assert_ne!(repo.resolve(&RefSpec::WorkingTree).unwrap().tree, wt.tree);
}

#[test]
fn working_tree_snapshot_rechecks_racy_index_entries_without_changing_real_index() {
    let t = RepoBuilder::new()
        .commit("one", files!["value.py" => "value = 1\n"])
        .build()
        .unwrap();
    // A fixed mtime recreates a same-timestamp edit without waiting for a clock
    // boundary. Ignore ctime because the fixture cannot restore filesystem ctime.
    t.git(&["config", "core.trustctime", "false"]).unwrap();
    let timestamp = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
    let file = t.write_file("value.py", b"value = 2\n").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(timestamp)
        .unwrap();
    t.git(&["add", "value.py"]).unwrap();
    let index = t.path().join(".git/index");
    std::fs::File::options()
        .write(true)
        .open(&index)
        .unwrap()
        .set_modified(timestamp)
        .unwrap();
    let original_index = std::fs::read(&index).unwrap();
    let original_mtime = std::fs::metadata(&index).unwrap().modified().unwrap();

    // The cached size and mtime still match, but Git must hash the file because
    // its mtime is at least as recent as the original index's mtime.
    t.write_file("value.py", b"value = 4\n").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_modified(timestamp)
        .unwrap();
    let repo = Repo::open(t.path()).unwrap();
    let current = repo.resolve(&RefSpec::WorkingTree).unwrap();
    let snapshot = repo.tree_snapshot(RepoId::nil(), current.tree).unwrap();
    let TreeEntryKind::File { oid, .. } = snapshot.entries[0].kind else {
        panic!("expected value.py file");
    };
    assert_eq!(repo.blob(oid).unwrap(), b"value = 4\n");
    let ResolvedSource::WorkingTree { dirty, .. } = current.source else {
        panic!("expected working tree");
    };
    assert_eq!(
        dirty
            .iter()
            .map(nits_protocol::RepoPath::as_str)
            .collect::<Vec<_>>(),
        vec!["value.py"]
    );
    assert_eq!(
        repo.resolve(&RefSpec::WorkingTree).unwrap().tree,
        current.tree
    );
    assert_eq!(std::fs::read(&index).unwrap(), original_index);
    assert_eq!(
        std::fs::metadata(&index).unwrap().modified().unwrap(),
        original_mtime
    );
    assert_eq!(t.git(&["show", ":value.py"]).unwrap(), "value = 2");
}

#[test]
fn working_tree_snapshot_without_index_supports_empty_and_untracked_unborn_repo() {
    let t = RepoBuilder::new().build().unwrap();
    let repo = Repo::open(t.path()).unwrap();
    let index = t.path().join(".git/index");
    assert!(!index.exists());

    let empty = repo.resolve(&RefSpec::WorkingTree).unwrap();
    assert!(
        repo.tree_snapshot(RepoId::nil(), empty.tree)
            .unwrap()
            .entries
            .is_empty()
    );
    assert_eq!(
        empty.source,
        ResolvedSource::WorkingTree {
            head: None,
            dirty: vec![],
            branch: Some("main".into())
        }
    );

    t.write_file(".gitignore", b"ignored.txt\n").unwrap();
    t.write_file("new.txt", b"untracked\n").unwrap();
    t.write_file("ignored.txt", b"excluded\n").unwrap();
    let current = repo.resolve(&RefSpec::WorkingTree).unwrap();
    let snapshot = repo.tree_snapshot(RepoId::nil(), current.tree).unwrap();
    let paths: Vec<_> = snapshot
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(paths, vec![".gitignore", "new.txt"]);
    let TreeEntryKind::File { oid, .. } = snapshot.entries[1].kind else {
        panic!("expected new.txt file");
    };
    assert_eq!(repo.blob(oid).unwrap(), b"untracked\n");
    let ResolvedSource::WorkingTree { dirty, .. } = current.source else {
        panic!("expected working tree");
    };
    assert_eq!(
        dirty
            .iter()
            .map(nits_protocol::RepoPath::as_str)
            .collect::<Vec<_>>(),
        paths
    );
    assert_eq!(
        repo.resolve(&RefSpec::WorkingTree).unwrap().tree,
        current.tree
    );
    assert!(!index.exists());
    assert_no_snapshot_indexes(&t);
}

#[test]
fn working_tree_snapshot_without_index_reflects_committed_repo_edits() {
    let t = RepoBuilder::new()
        .commit("one", files!["a.txt" => "old\n", "deleted.txt" => "gone\n", ".gitignore" => "ignored.txt\n"])
        .write(files!["a.txt" => "current\n", "new.txt" => "untracked\n", "ignored.txt" => "excluded\n"])
        .remove(&["deleted.txt"])
        .build()
        .unwrap();
    let index = t.path().join(".git/index");
    std::fs::remove_file(&index).unwrap();
    let repo = Repo::open(t.path()).unwrap();
    let current = repo.resolve(&RefSpec::WorkingTree).unwrap();
    let snapshot = repo.tree_snapshot(RepoId::nil(), current.tree).unwrap();
    let paths: Vec<_> = snapshot
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    assert_eq!(paths, vec![".gitignore", "a.txt", "new.txt"]);
    let TreeEntryKind::File { oid, .. } = snapshot.entries[1].kind else {
        panic!("expected a.txt file");
    };
    assert_eq!(repo.blob(oid).unwrap(), b"current\n");
    let ResolvedSource::WorkingTree { dirty, .. } = current.source else {
        panic!("expected working tree");
    };
    assert_eq!(
        dirty
            .iter()
            .map(nits_protocol::RepoPath::as_str)
            .collect::<Vec<_>>(),
        vec!["a.txt", "deleted.txt", "new.txt"]
    );
    assert_eq!(
        repo.resolve(&RefSpec::WorkingTree).unwrap().tree,
        current.tree
    );
    assert!(!index.exists());
    assert_no_snapshot_indexes(&t);
}

#[test]
fn working_tree_snapshot_without_index_cleans_up_after_git_failure() {
    let t = RepoBuilder::new()
        .write(files![".gitattributes" => "a.txt filter=snapshot-test\n", "a.txt" => "content\n"])
        .build()
        .unwrap();
    t.git(&[
        "config",
        "filter.snapshot-test.clean",
        "git nits-no-such-clean-filter",
    ])
    .unwrap();
    t.git(&["config", "filter.snapshot-test.required", "true"])
        .unwrap();
    let repo = Repo::open(t.path()).unwrap();
    let error = repo.resolve(&RefSpec::WorkingTree).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("clean filter 'snapshot-test' failed"),
        "{error}"
    );
    assert!(!t.path().join(".git/index").exists());
    assert_no_snapshot_indexes(&t);
}

fn assert_no_snapshot_indexes(t: &TestRepo) {
    let artifacts: Vec<_> = std::fs::read_dir(t.path().join(".git"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().starts_with("nits-index-"))
        .collect();
    assert!(
        artifacts.is_empty(),
        "snapshot index artifacts remain: {artifacts:?}"
    );
    assert!(!t.path().join(".git/index.lock").exists());
}

#[test]
fn tree_delta_between_snapshots() {
    let t = RepoBuilder::new()
        .commit("one", files!["a.txt" => "a\n", "b.txt" => "b\n"])
        .build()
        .unwrap();
    let repo = Repo::open(t.path()).unwrap();
    let before = repo.resolve(&RefSpec::WorkingTree).unwrap().tree;
    t.write_file("a.txt", b"a2\n").unwrap();
    t.write_file("d/new.txt", b"n\n").unwrap();
    std::fs::remove_file(t.path().join("b.txt")).unwrap();
    let after = repo.resolve(&RefSpec::WorkingTree).unwrap().tree;
    let delta = repo.tree_delta(RepoId::nil(), before, after).unwrap();
    assert_eq!(
        delta
            .added
            .iter()
            .map(|e| e.path.as_str())
            .collect::<Vec<_>>(),
        vec!["d/new.txt"]
    );
    assert_eq!(
        delta
            .removed
            .iter()
            .map(nits_protocol::RepoPath::as_str)
            .collect::<Vec<_>>(),
        vec!["b.txt"]
    );
    assert_eq!(
        delta
            .changed
            .iter()
            .map(|e| e.path.as_str())
            .collect::<Vec<_>>(),
        vec!["a.txt"]
    );
}

#[test]
fn commits_between_carries_full_message_and_signatures_incl_merges() {
    let t = RepoBuilder::new()
        .commit("base", files!["a.txt" => "a\n"])
        .branch("feature")
        .commit(
            "feat: add b\n\nLonger body here.\n\nSecond paragraph.",
            files!["b.txt" => "b\n"],
        )
        .checkout("main")
        .commit("main moves", files!["c.txt" => "c\n"])
        .build()
        .unwrap();
    t.git(&["merge", "--no-ff", "-q", "-m", "merge feature", "feature"])
        .unwrap();
    let repo = Repo::open(t.path()).unwrap();
    let base = repo.rev_parse_commit("feature~1").unwrap();
    let head = repo.rev_parse_commit("main").unwrap();
    let commits = repo.commits_between(base, head).unwrap();
    // Topo order: the merge first; the two sides are siblings and git may
    // emit either side first.
    assert_eq!(commits[0].subject, "merge feature");
    assert_eq!(commits[0].parents.len(), 2, "merge has two parents");
    let mut sides: Vec<&str> = commits[1..].iter().map(|c| c.subject.as_str()).collect();
    sides.sort_unstable();
    assert_eq!(sides, vec!["feat: add b", "main moves"]);
    let feat = commits.iter().find(|c| c.subject == "feat: add b").unwrap();
    assert_eq!(feat.body, "Longer body here.\n\nSecond paragraph.");
    assert_eq!(feat.author.name, "Test User");
    assert_eq!(feat.author.email, "test@example.com");
    assert_eq!(feat.author.time.millis(), 1_704_067_200_000);
    assert_eq!(feat.author.offset_minutes, 0);
    assert_eq!(
        feat.tree.to_string(),
        t.git(&["rev-parse", "feature^{tree}"]).unwrap()
    );
}

#[test]
fn binary_detection_and_blob_read() {
    assert!(is_binary(b"abc\0def"));
    assert!(!is_binary(b"plain text\n"));
    let t = RepoBuilder::new()
        .commit("bin", files!["img.png" => b"\x89PNG\0\0\x1a".as_slice()])
        .build()
        .unwrap();
    let repo = Repo::open(t.path()).unwrap();
    let head = repo.resolve(&RefSpec::Head).unwrap();
    let changes = repo
        .changed_files(
            TreeOid::new("4b825dc642cb6eb9a060e54bf8d69288fbee4904".parse().unwrap()),
            head.tree,
        )
        .unwrap();
    let ChangeKind::Added { new } = changes[0].kind else {
        panic!()
    };
    assert!(is_binary(&repo.blob(new).unwrap()));
}
