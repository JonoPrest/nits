//! Suggestion application against real git-generated patches and durable Core state.

use nits_protocol::{
    Anchor, Author, BlobOid, ClientId, ClientSeq, CommentId, CommentKind, EventBody, NonEmpty,
    RefSpec, RepoId, RepoPath, ReviewId, ReviewTarget, Timestamp, WorkspaceId,
};
use nits_review_core::{Core, CoreError, Ctx, DataDir};
use nits_test_support::{RepoBuilder, TestRepo, files};
use std::process::Command;

struct Suggestion {
    _data: tempfile::TempDir,
    repo: TestRepo,
    core: Core,
    ctx: Ctx,
    review: ReviewId,
    repo_id: RepoId,
    blob: BlobOid,
}

impl Suggestion {
    fn new(original: &[u8]) -> Self {
        let repo = RepoBuilder::new()
            .commit(
                "original",
                files![".gitattributes" => "file.txt -text\n", "file.txt" => original],
            )
            .build()
            .unwrap();
        repo.git(&["config", "core.autocrlf", "false"]).unwrap();
        let data = tempfile::tempdir().unwrap();
        let core = Core::open(&DataDir::new(data.path().to_path_buf())).unwrap();
        let ctx = Ctx {
            author: Author::Human {
                name: "ada".into(),
                machine: "box".into(),
            },
            client_id: ClientId::from_parts(1, 1),
            client_seq: ClientSeq::new(0),
            now: Timestamp::from_millis(1_700_000_000_000),
        };
        let workspace = WorkspaceId::from_parts(2, 1);
        let repo_id = RepoId::from_parts(3, 1);
        let review = ReviewId::from_parts(4, 1);
        core.create_workspace(&ctx, workspace, "patches".into())
            .unwrap();
        core.attach_repo(
            &ctx,
            workspace,
            repo_id,
            repo.path().to_str().unwrap(),
            "repo".into(),
        )
        .unwrap();
        core.create_review(
            &ctx,
            review,
            workspace,
            "suggestions".into(),
            NonEmpty::new(vec![ReviewTarget {
                repo_id,
                base: RefSpec::Head,
                head: RefSpec::WorkingTree,
            }])
            .unwrap(),
        )
        .unwrap();
        let blob = repo
            .git(&["rev-parse", "HEAD:file.txt"])
            .unwrap()
            .parse()
            .unwrap();
        Self {
            _data: data,
            repo,
            core,
            ctx,
            review,
            repo_id,
            blob,
        }
    }

    fn add(&self, id: CommentId, patch: &str) {
        self.core
            .add_comment(
                &self.ctx,
                self.review,
                id,
                CommentKind::Suggestion {
                    patch: patch.into(),
                },
                Anchor::File {
                    repo_id: self.repo_id,
                    path: RepoPath::new("file.txt").unwrap(),
                    blob_oid: self.blob,
                },
                "suggestion".into(),
                None,
            )
            .unwrap();
    }

    fn git_patch(&self, expected: &[u8], context: &str) -> String {
        self.repo.write_file("file.txt", expected).unwrap();
        // TestRepo::git trims stdout: use raw output to preserve the patch's
        // final LF and CRLF source bytes, just as an external producer would.
        let result = Command::new("git")
            .current_dir(self.repo.path())
            .args([
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                context,
                "--",
                "file.txt",
            ])
            .output()
            .unwrap();
        assert!(result.status.success());
        let diff = String::from_utf8(result.stdout).unwrap();
        let headers = diff.find("\n--- ").unwrap();
        diff[headers + 1..].to_owned()
    }
}

#[test]
fn git_generated_patches_apply_exact_bytes_and_record_the_result() {
    let cases: &[(&str, &[u8], &[u8])] = &[
        ("insert BOF", b"a\nb\nc\n", b"added\na\nb\nc\n"),
        ("insert middle", b"a\nb\nc\n", b"a\nb\nadded\nc\n"),
        ("insert EOF", b"a\nb\nc\n", b"a\nb\nc\nadded\n"),
        ("insert empty file", b"", b"added\n"),
        ("delete middle", b"a\nb\nc\n", b"a\nc\n"),
        ("delete all", b"a\nb\n", b""),
        (
            "multiple shifted hunks",
            b"a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n",
            b"first\na\nb\nc\nd\ne\nf\ng\nh\ni\nlast\nextra\n",
        ),
        ("CRLF", b"a\r\nb\r\nc\r\n", b"a\r\nB\r\nc\r\n"),
        ("mixed terminators", b"a\r\nb\nc\r\n", b"a\r\nB\nc\r\n"),
        ("explicit CRLF conversion", b"a\r\nb\r\n", b"a\nb\n"),
        ("replace without newline", b"a\nb", b"a\nB"),
        ("add final newline", b"a\nb", b"a\nb\n"),
        ("remove final newline", b"a\nb\n", b"a\nb"),
        ("insert before unterminated EOF", b"a\nb", b"a\nadded\nb"),
        ("insert after unterminated EOF", b"a\nb", b"a\nb\nadded"),
        ("delete unterminated EOF", b"a\nb", b"a\n"),
        ("empty to unterminated", b"", b"added"),
        ("unterminated to empty", b"a", b""),
        ("CR at unterminated EOF", b"a\r", b"b\r"),
    ];
    for &(name, original, expected) in cases {
        for context in ["--unified=0", "--unified=3"] {
            let fixture = Suggestion::new(original);
            let patch = fixture.git_patch(expected, context);
            fixture.repo.write_file("file.txt", original).unwrap();
            let id = CommentId::from_parts(5, 1);
            fixture.add(id, &patch);
            let before = fixture.core.last_seq().unwrap();
            let result = fixture
                .core
                .apply_suggestion(&fixture.ctx, fixture.review, id)
                .unwrap_or_else(|error| panic!("{name}, {context}: {error}\n{patch:?}"));
            assert_eq!(
                std::fs::read(fixture.repo.path().join("file.txt")).unwrap(),
                expected,
                "{name}, {context}"
            );
            assert_eq!(
                fixture.core.repo_blob(fixture.repo_id, result).unwrap(),
                expected,
                "{name}, {context}"
            );
            let events = fixture.core.events_after(before).unwrap();
            assert_eq!(events.len(), 1, "{name}, {context}");
            assert!(
                matches!(events[0].body, EventBody::SuggestionApplied { result_blob, .. } if result_blob == result)
            );
        }
    }
}

#[test]
fn malformed_patches_leave_file_and_log_untouched_then_valid_suggestion_succeeds() {
    let original = b"a\r\nb\r\nc\r\n";
    let fixture = Suggestion::new(original);
    let invalid = [
        "@@ -2,1 +2,1 @@\n-b\n+B\n",           // LF cannot match CRLF.
        "@@ -2,1 +2,2 @@\n-b\r\n+B\r\n",       // New count.
        "@@ -2,2 +2,1 @@\n-b\r\n+B\r\n",       // Old count.
        "@@ -2,1 +3,1 @@\n-b\r\n+B\r\n",       // New position.
        "@@ -4,1 +4,1 @@\n-missing\r\n+B\r\n", // Beyond EOF.
        "@@ -0,1 +1,1 @@\n-a\r\n+A\r\n",       // Nonempty line zero.
        "@@ -2,0 +3,1 @@\n+added\n\\ No newline at end of file\n", // Unterminated insertion in middle.
        "@@ -2,1 +2,1 @@\n-b\r\n+B\r\n@@ -1,1 +1,1 @@\n-a\r\n+A\r\n", // Overlapping hunks.
        "@@ -2,0 +3,1 @@\n+added\r\n@@ -3,1 +3,1 @@\n-c\r\n+C\r\n", // Missing cumulative new offset.
        "@@ -2,1 +2,1 @@\n-b\r\n+B",                                // Truncated patch.
        "@@ -2,1 +2,1 @@\n-b\r\n+B\r\n@@ garbage\n", // Late parse error after valid edit.
    ];
    for (index, patch) in invalid.iter().enumerate() {
        let id = CommentId::from_parts(5, index as u128);
        fixture.add(id, patch);
        let before = fixture.core.last_seq().unwrap();
        let error = fixture
            .core
            .apply_suggestion(&fixture.ctx, fixture.review, id)
            .unwrap_err();
        assert!(
            matches!(error, CoreError::Invalid { .. }),
            "{patch:?}: {error}"
        );
        assert_eq!(
            std::fs::read(fixture.repo.path().join("file.txt")).unwrap(),
            original,
            "{patch:?}"
        );
        assert_eq!(fixture.core.last_seq().unwrap(), before, "{patch:?}");
        assert!(
            fixture.core.events_after(before).unwrap().is_empty(),
            "{patch:?}"
        );
    }
    let id = CommentId::from_parts(5, 100);
    fixture.add(id, "@@ -2,1 +2,1 @@\n-b\r\n+B\r\n");
    fixture
        .core
        .apply_suggestion(&fixture.ctx, fixture.review, id)
        .unwrap();
    assert_eq!(
        std::fs::read(fixture.repo.path().join("file.txt")).unwrap(),
        b"a\r\nB\r\nc\r\n"
    );
}

#[test]
fn linked_suggestion_targets_never_modify_outside_bytes_or_append_success() {
    use std::os::unix::fs::symlink;
    for kind in ["file symlink", "parent symlink", "hard link"] {
        let fixture = Suggestion::new(b"base\n");
        let outside = tempfile::tempdir().unwrap();
        let victim = outside.path().join("file.txt");
        std::fs::write(&victim, b"base\n").unwrap();
        let path = if kind == "parent symlink" {
            let dir = fixture.repo.path().join("dir");
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("file.txt"), b"base\n").unwrap();
            RepoPath::new("dir/file.txt").unwrap()
        } else {
            RepoPath::new("file.txt").unwrap()
        };
        let id = CommentId::from_parts(5, 1);
        fixture
            .core
            .add_comment(
                &fixture.ctx,
                fixture.review,
                id,
                CommentKind::Suggestion {
                    patch: "@@ -1 +1 @@\n-base\n+changed\n".into(),
                },
                Anchor::File {
                    repo_id: fixture.repo_id,
                    path: path.clone(),
                    blob_oid: fixture.blob,
                },
                "linked target".into(),
                None,
            )
            .unwrap();
        let target = fixture.repo.path().join(path.as_str());
        if kind == "parent symlink" {
            std::fs::remove_dir_all(fixture.repo.path().join("dir")).unwrap();
            symlink(outside.path(), fixture.repo.path().join("dir")).unwrap();
        } else {
            std::fs::remove_file(&target).unwrap();
            if kind == "file symlink" {
                symlink(&victim, &target).unwrap();
            } else {
                std::fs::hard_link(&victim, &target).unwrap();
            }
        }
        let before = fixture.core.last_seq().unwrap();
        let error = fixture
            .core
            .apply_suggestion(&fixture.ctx, fixture.review, id)
            .unwrap_err();
        assert!(
            matches!(error, CoreError::Invalid { .. }),
            "{kind}: {error}"
        );
        assert_eq!(std::fs::read(&victim).unwrap(), b"base\n", "{kind}");
        assert_eq!(std::fs::read(&target).unwrap(), b"base\n", "{kind}");
        assert_eq!(fixture.core.last_seq().unwrap(), before, "{kind}");
        assert!(fixture.core.events_after(before).unwrap().is_empty());
    }
}

#[test]
fn executable_suggestion_preserves_permissions_and_exact_unterminated_crlf_bytes() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let fixture = Suggestion::new(b"base\r\nlast");
    let file = fixture.repo.path().join("file.txt");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o751)).unwrap();
    let id = CommentId::from_parts(5, 1);
    fixture.add(id, "@@ -1 +1 @@\n-base\r\n+changed\r\n");
    fixture
        .core
        .apply_suggestion(&fixture.ctx, fixture.review, id)
        .unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), b"changed\r\nlast");
    assert_eq!(std::fs::metadata(&file).unwrap().mode() & 0o777, 0o751);
    assert!(
        !std::fs::read_dir(fixture.repo.path().join(".git"))
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("nits-suggestion-"))
    );
}
