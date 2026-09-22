//! A pushed author clone and independent reviewer checkout: no checkout mutation.
use nits_protocol::{
    AgentVia, Author, ClientId, ClientSeq, CommitOid, CreateReviewTargets, FetchResolution,
    NonEmpty, RefSpec, RemoteName, RepoId, RequestCheckpointComparison, RequestedTargets,
    ResolvedSource, ReviewId, ReviewRequest, ReviewTarget, SymbolicTrackingRef, TargetComparison,
    Timestamp, WorkspaceId,
};
use nits_review_core::git::Repo;
use nits_review_core::{Core, Ctx, DataDir};
use nits_test_support::{RepoBuilder, TestRepo, files};
use std::path::{Path, PathBuf};

fn git(path: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim_end().to_owned()
}

fn ctx() -> Ctx {
    Ctx {
        author: Author::Human {
            name: "Ada".into(),
            machine: "author-box".into(),
        },
        client_id: ClientId::from_parts(1, 1),
        client_seq: ClientSeq::new(1),
        now: Timestamp::from_millis(1),
    }
}

struct World {
    temp: tempfile::TempDir,
    author: TestRepo,
    checkout: PathBuf,
    core: Core,
    review: ReviewId,
    repo: RepoId,
    base: CommitOid,
}

fn world(linked: bool, head: &str) -> World {
    let temp = tempfile::tempdir().unwrap();
    let author = RepoBuilder::new()
        .commit(
            "base",
            files!["file.txt" => "base\n", "staged.txt" => "base\n"],
        )
        .tag("v1")
        .build()
        .unwrap();
    let base = author.rev_parse("HEAD").unwrap().parse().unwrap();
    let remote = temp.path().join("remote.git");
    git(
        temp.path(),
        &[
            "clone",
            "-q",
            "--bare",
            author.path().to_str().unwrap(),
            remote.to_str().unwrap(),
        ],
    );
    author
        .git(&["remote", "add", "origin", remote.to_str().unwrap()])
        .unwrap();
    let main = temp.path().join("reviewer");
    git(
        temp.path(),
        &[
            "clone",
            "-q",
            remote.to_str().unwrap(),
            main.to_str().unwrap(),
        ],
    );
    let checkout = if linked {
        let checkout = temp.path().join("linked-reviewer");
        git(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "linked",
                checkout.to_str().unwrap(),
            ],
        );
        checkout
    } else {
        main
    };
    git(&checkout, &["branch", "local-copy"]);
    let core = Core::open(&DataDir::new(temp.path().join("data"))).unwrap();
    let workspace = WorkspaceId::from_parts(1, 2);
    let repo = RepoId::from_parts(1, 3);
    let review = ReviewId::from_parts(1, 4);
    core.create_workspace(&ctx(), workspace, "shared".into())
        .unwrap();
    core.attach_repo(
        &ctx(),
        workspace,
        repo,
        checkout.to_str().unwrap(),
        "reviewer".into(),
    )
    .unwrap();
    core.create_review(
        &ctx(),
        review,
        workspace,
        "round".into(),
        CreateReviewTargets::singleton(ReviewTarget {
            repo_id: repo,
            base: RefSpec::Commit { oid: base },
            head: head.parse().unwrap(),
        }),
    )
    .unwrap();
    World {
        temp,
        author,
        checkout,
        core,
        review,
        repo,
        base,
    }
}

fn pushed(w: &World, empty: bool) -> CommitOid {
    if empty {
        w.author
            .git(&["commit", "-q", "--allow-empty", "-m", "empty change"])
            .unwrap();
    } else {
        w.author
            .write_file("file.txt", b"updated by author\n")
            .unwrap();
        w.author.git(&["add", "file.txt"]).unwrap();
        w.author
            .git(&["commit", "-q", "-m", "fix typo with spaces"])
            .unwrap();
    }
    w.author.git(&["push", "-q", "origin", "main"]).unwrap();
    w.author.rev_parse("HEAD").unwrap().parse().unwrap()
}

#[test]
fn fetch_preserves_checkout_index_and_local_refs_even_with_hostile_configured_mappings() {
    for linked in [false, true] {
        for selected in ["HEAD", "origin/main"] {
            let w = world(linked, selected);
            std::fs::write(w.checkout.join("staged.txt"), b"staged\n").unwrap();
            git(&w.checkout, &["add", "staged.txt"]);
            std::fs::write(w.checkout.join("file.txt"), b"unstaged\n").unwrap();
            std::fs::write(w.checkout.join("untracked.txt"), b"untracked\n").unwrap();
            git(
                &w.checkout,
                &["config", "--unset-all", "remote.origin.fetch"],
            );
            for mapping in [
                "+refs/heads/main:refs/heads/main",
                "+refs/heads/main:refs/heads/local-copy",
                "+refs/heads/main:refs/heads/linked",
            ] {
                git(
                    &w.checkout,
                    &["config", "--add", "remote.origin.fetch", mapping],
                );
            }
            let index = w
                .checkout
                .join(git(&w.checkout, &["rev-parse", "--git-path", "index"]));
            let index_before = std::fs::read(&index).unwrap();
            let refs_before = git(
                &w.checkout,
                &[
                    "for-each-ref",
                    "--format=%(refname) %(objectname)",
                    "refs/heads",
                ],
            );
            let head_before = git(&w.checkout, &["rev-parse", "HEAD"]);
            let head_name = git(&w.checkout, &["symbolic-ref", "HEAD"]);
            let next = pushed(&w, false);
            assert!(
                Repo::open(&w.checkout)
                    .unwrap()
                    .resolve(&RefSpec::Commit { oid: next })
                    .is_err()
            );
            let result = w
                .core
                .fetch_review(&ctx(), w.review, None, RemoteName::default())
                .unwrap();
            let FetchResolution::Resolved { targets, changed } = result.resolution else {
                panic!("refresh failed")
            };
            assert_eq!(changed, selected == "origin/main");
            assert_eq!(
                targets.first().head.source,
                ResolvedSource::Commit {
                    oid: if selected == "HEAD" { w.base } else { next }
                }
            );
            assert_eq!(
                git(&w.checkout, &["rev-parse", "origin/main"]),
                next.to_string()
            );
            assert_eq!(
                git(
                    &w.checkout,
                    &[
                        "for-each-ref",
                        "--format=%(refname) %(objectname)",
                        "refs/heads"
                    ]
                ),
                refs_before
            );
            assert_eq!(git(&w.checkout, &["rev-parse", "HEAD"]), head_before);
            assert_eq!(git(&w.checkout, &["symbolic-ref", "HEAD"]), head_name);
            assert_eq!(std::fs::read(&index).unwrap(), index_before);
            assert_eq!(
                std::fs::read(w.checkout.join("file.txt")).unwrap(),
                b"unstaged\n"
            );
            assert_eq!(
                std::fs::read(w.checkout.join("staged.txt")).unwrap(),
                b"staged\n"
            );
            assert_eq!(
                std::fs::read(w.checkout.join("untracked.txt")).unwrap(),
                b"untracked\n"
            );
            assert_eq!(
                w.core.review(w.review).unwrap().review.targets.first().head,
                selected.parse().unwrap()
            );
        }
    }
}

#[test]
fn git_revision_expressions_resolve_available_commits_and_report_unavailable_input() {
    let w = world(false, "HEAD");
    let next = pushed(&w, false);
    w.core
        .fetch_review(&ctx(), w.review, None, RemoteName::default())
        .unwrap();
    git(
        &w.checkout,
        &[
            "-c",
            "user.name=Ada",
            "-c",
            "user.email=ada@example.com",
            "tag",
            "-a",
            "annotated-base",
            "-m",
            "base tag",
            "HEAD",
        ],
    );
    let repo = Repo::open(&w.checkout).unwrap();
    for (text, expected) in [
        (next.to_string(), next),
        (next.to_string()[..8].to_owned(), next),
        ("origin/main".into(), next),
        ("refs/remotes/origin/main".into(), next),
        ("origin/main~1".into(), w.base),
        (":/fix typo with spaces".into(), next),
        (":/^base".into(), w.base),
        ("origin/main^{/base}".into(), w.base),
        ("v1".into(), w.base),
        ("tag:v1".into(), w.base),
        ("annotated-base".into(), w.base),
        ("branch:main".into(), w.base),
    ] {
        if !text.starts_with("tag:") && !text.starts_with("branch:") {
            let object = git(
                &w.checkout,
                &["rev-parse", "--verify", "--end-of-options", &text],
            );
            assert_eq!(
                git(
                    &w.checkout,
                    &["rev-parse", "--verify", &format!("{object}^{{commit}}")]
                ),
                expected.to_string(),
                "raw Git control for {text}"
            );
        }
        assert_eq!(
            repo.resolve(&text.parse().unwrap()).unwrap().source,
            ResolvedSource::Commit { oid: expected },
            "{text}"
        );
    }
    for text in [
        "missing-branch",
        "HEAD:file.txt",
        "HEAD^{tree}",
        "HEAD..origin/main",
        "--all",
    ] {
        let error = repo
            .resolve(&text.parse().unwrap())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(text)
                && error.contains(w.checkout.to_str().unwrap())
                && error.contains("Fetch explicitly"),
            "{error}"
        );
    }
    let candidates = repo.ref_candidates().unwrap();
    assert!(
        candidates
            .iter()
            .any(|candidate| candidate.ref_spec == "refs/remotes/origin/main".parse().unwrap())
    );
}

#[test]
fn fetch_rejects_unrelated_members_and_reports_resolution_failure_after_success() {
    let w = world(false, "branch:local-copy");
    let before = w.core.last_seq().unwrap();
    for (repo, remote) in [
        (Some(RepoId::from_parts(9, 9)), RemoteName::default()),
        (None, "missing".parse().unwrap()),
    ] {
        assert!(w.core.fetch_review(&ctx(), w.review, repo, remote).is_err());
        assert_eq!(w.core.last_seq().unwrap(), before);
    }
    let next = pushed(&w, false);
    git(&w.checkout, &["branch", "-D", "local-copy"]);
    let fetched = w
        .core
        .fetch_review(&ctx(), w.review, Some(w.repo), RemoteName::default())
        .unwrap();
    assert!(matches!(
        fetched.resolution,
        FetchResolution::Unavailable { .. }
    ));
    assert_eq!(
        git(&w.checkout, &["rev-parse", "origin/main"]),
        next.to_string()
    );
    assert_eq!(w.core.last_seq().unwrap(), before);
}

#[test]
fn fetch_reports_symbolic_tracking_aliases_without_claiming_they_advanced() {
    let w = world(false, "origin/main");
    git(
        &w.checkout,
        &[
            "symbolic-ref",
            "refs/remotes/origin/main",
            "refs/heads/main",
        ],
    );
    let next = pushed(&w, false);
    let before_head = git(&w.checkout, &["rev-parse", "HEAD"]);
    let before_index = std::fs::read(w.checkout.join(".git/index")).unwrap();
    let result = w
        .core
        .fetch_review(&ctx(), w.review, None, RemoteName::default())
        .unwrap();
    assert!(
        result
            .symbolic_tracking_refs
            .contains(&SymbolicTrackingRef {
                name: "refs/remotes/origin/main".into(),
                target: "refs/heads/main".into()
            })
    );
    let FetchResolution::Resolved { targets, changed } = result.resolution else {
        panic!("alias resolves")
    };
    assert!(!changed);
    assert_eq!(
        targets.first().head.source,
        ResolvedSource::Commit { oid: w.base }
    );
    assert_ne!(
        targets.first().head.source,
        ResolvedSource::Commit { oid: next }
    );
    assert_eq!(
        git(&w.checkout, &["symbolic-ref", "refs/remotes/origin/main"]),
        "refs/heads/main"
    );
    assert_eq!(git(&w.checkout, &["rev-parse", "HEAD"]), before_head);
    assert_eq!(
        std::fs::read(w.checkout.join(".git/index")).unwrap(),
        before_index
    );
}

fn request(w: &World) -> ReviewRequest {
    w.core
        .request_review(&ctx(), w.review, "reviewer".into(), "Please check".into())
        .unwrap();
    w.core
        .review_snapshot(w.review)
        .unwrap()
        .requests
        .last()
        .unwrap()
        .clone()
}

#[test]
fn request_warning_binds_to_latest_checkpoint_and_survives_reopen() {
    let w = world(false, "origin/main");
    assert_eq!(
        request(&w).checkpoint_comparison,
        RequestCheckpointComparison::NoCheckpoint
    );
    let first = w
        .core
        .record_checkpoint(
            &ctx(),
            w.review,
            w.core.review_snapshot(w.review).unwrap().resolved.unwrap(),
            None,
        )
        .unwrap();
    let same = request(&w);
    assert_eq!(
        same.checkpoint_comparison,
        RequestCheckpointComparison::Compared {
            checkpoint_id: first,
            outcome: TargetComparison::SameTargets
        }
    );
    let next = pushed(&w, true);
    // The remote commit does not exist in the reviewer checkout until explicit fetch.
    assert_eq!(
        request(&w).checkpoint_comparison,
        same.checkpoint_comparison
    );
    w.core
        .fetch_review(&ctx(), w.review, None, RemoteName::default())
        .unwrap();
    let changed = request(&w);
    assert_eq!(
        changed.checkpoint_comparison,
        RequestCheckpointComparison::Compared {
            checkpoint_id: first,
            outcome: TargetComparison::ChangedTargets
        }
    );
    let snapshot = w.core.review_snapshot(w.review).unwrap();
    assert_eq!(
        snapshot.resolved.as_ref().unwrap().first().head.source,
        ResolvedSource::Commit { oid: next }
    );
    let second = w
        .core
        .record_checkpoint(
            &Ctx {
                author: Author::Agent {
                    name: "second-reviewer".into(),
                    model: "test".into(),
                    session_id: "round-2".into(),
                    invoked_by: None,
                    via: AgentVia::Mcp,
                },
                ..ctx()
            },
            w.review,
            snapshot.resolved.unwrap(),
            None,
        )
        .unwrap();
    assert_eq!(
        request(&w).checkpoint_comparison,
        RequestCheckpointComparison::Compared {
            checkpoint_id: second,
            outcome: TargetComparison::SameTargets
        }
    );
    let expected = w.core.review_snapshot(w.review).unwrap();
    drop(w.core);
    let reopened = Core::open(&DataDir::new(w.temp.path().join("data"))).unwrap();
    assert_eq!(reopened.review_snapshot(w.review).unwrap(), expected);
    assert_eq!(
        expected.requests[1], same,
        "later checks never rewrite historical warnings"
    );
}

#[test]
fn request_warning_sees_working_tree_head_changes_even_when_content_is_identical() {
    let w = world(false, "worktree");
    let old = w.core.review_snapshot(w.review).unwrap().resolved.unwrap();
    let checkpoint = w
        .core
        .record_checkpoint(&ctx(), w.review, old.clone(), None)
        .unwrap();
    assert_eq!(
        request(&w).checkpoint_comparison,
        RequestCheckpointComparison::Compared {
            checkpoint_id: checkpoint,
            outcome: TargetComparison::SameTargets
        }
    );
    git(
        &w.checkout,
        &[
            "-c",
            "user.name=Ada",
            "-c",
            "user.email=ada@example.com",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "same tree, new HEAD",
        ],
    );
    let changed = request(&w);
    let RequestedTargets::Captured { targets } = changed.targets else {
        panic!("new capture")
    };
    assert_eq!(targets.first().head.tree, old.first().head.tree);
    assert_eq!(
        changed.checkpoint_comparison,
        RequestCheckpointComparison::Compared {
            checkpoint_id: checkpoint,
            outcome: TargetComparison::ChangedTargets
        }
    );
}

#[test]
fn fetch_requires_explicit_member_in_multi_repository_reviews() {
    let w = world(false, "origin/main");
    let workspace = w.core.review(w.review).unwrap().review.workspace_id;
    let other_repo = RepoId::from_parts(3, 1);
    w.core
        .attach_repo(
            &ctx(),
            workspace,
            other_repo,
            w.author.path().to_str().unwrap(),
            "author".into(),
        )
        .unwrap();
    let review = ReviewId::from_parts(3, 2);
    let mut targets = w.core.review(w.review).unwrap().review.targets;
    targets.push(ReviewTarget {
        repo_id: other_repo,
        base: RefSpec::Commit { oid: w.base },
        head: RefSpec::Commit { oid: w.base },
    });
    w.core
        .create_review(
            &ctx(),
            review,
            workspace,
            "multi".into(),
            CreateReviewTargets::try_from(targets).unwrap(),
        )
        .unwrap();
    let old = w.core.review_snapshot(review).unwrap().resolved.unwrap();
    let checkpoint = w
        .core
        .record_checkpoint(&ctx(), review, old.clone(), None)
        .unwrap();
    let mut reordered: Vec<_> = old.clone().into();
    reordered.reverse();
    assert_eq!(
        RequestCheckpointComparison::against(
            Some(&w.core.review_snapshot(review).unwrap().checkpoints[0]),
            &NonEmpty::new(reordered).unwrap()
        ),
        RequestCheckpointComparison::Compared {
            checkpoint_id: checkpoint,
            outcome: TargetComparison::SameTargets
        }
    );
    let next = pushed(&w, false);
    let before = w.core.last_seq().unwrap();
    assert!(
        w.core
            .fetch_review(&ctx(), review, None, RemoteName::default())
            .unwrap_err()
            .to_string()
            .contains("multiple repositories")
    );
    assert_eq!(w.core.last_seq().unwrap(), before);
    assert_eq!(
        git(&w.checkout, &["rev-parse", "origin/main"]),
        w.base.to_string()
    );
    let fetched = w
        .core
        .fetch_review(&ctx(), review, Some(w.repo), RemoteName::default())
        .unwrap();
    let FetchResolution::Resolved { targets, .. } = fetched.resolution else {
        panic!("resolved")
    };
    assert_eq!(
        targets.first().head.source,
        ResolvedSource::Commit { oid: next }
    );
    w.core
        .request_review(
            &ctx(),
            review,
            "reviewer".into(),
            "updated one repository".into(),
        )
        .unwrap();
    assert_eq!(
        w.core.review_snapshot(review).unwrap().requests[0].checkpoint_comparison,
        RequestCheckpointComparison::Compared {
            checkpoint_id: checkpoint,
            outcome: TargetComparison::ChangedTargets
        }
    );
}

#[test]
fn historical_missing_working_tree_head_does_not_invent_a_revision_change() {
    let w = world(false, "worktree");
    let targets = w.core.review_snapshot(w.review).unwrap().resolved.unwrap();
    w.core
        .record_checkpoint(&ctx(), w.review, targets.clone(), None)
        .unwrap();
    let mut historical = w
        .core
        .review_snapshot(w.review)
        .unwrap()
        .checkpoints
        .remove(0);
    let ResolvedSource::WorkingTree { head, .. } =
        &mut historical.targets.iter_mut().next().unwrap().head.source
    else {
        panic!("working tree")
    };
    *head = None;
    assert_eq!(
        RequestCheckpointComparison::against(Some(&historical), &targets),
        RequestCheckpointComparison::Compared {
            checkpoint_id: historical.id,
            outcome: TargetComparison::UnknownRevision
        }
    );
    std::fs::write(w.checkout.join("file.txt"), b"new content\n").unwrap();
    let (changed, _) = w.core.resolve_targets(&ctx(), w.review).unwrap();
    assert_eq!(
        RequestCheckpointComparison::against(Some(&historical), &changed),
        RequestCheckpointComparison::Compared {
            checkpoint_id: historical.id,
            outcome: TargetComparison::ChangedTargets
        }
    );
}
