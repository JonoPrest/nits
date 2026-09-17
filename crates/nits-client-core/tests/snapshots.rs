//! Plan 3.5: `insta` snapshots of the whole `ViewModel` after scenario
//! scripts, so a change to what hosts render is a reviewed diff.
//!
//! Scripts drive a single core through a local (streamed) open with two
//! files, comments, a viewport, a draft and the help overlay. Everything
//! is deterministic: fixed ids, a fixed clock, no randomness.

// The fixture reads top to bottom.
#![allow(clippy::too_many_lines)]

use nits_client_core::{
    Action, CacheConfig, ClientCore, Config, Effect, Focus, IdSeed, Input, KeyChord, TransportEvent,
};
use nits_protocol::{
    Anchor, Author, BuildInfo, ChangeKind, ChunkIndex, ClientId, ClientMsg, ClientSeq, Comment,
    CommentId, CommentKind, CommentState, CommitInfo, CommitOid, ContextHash, Event, EventBody,
    FileRenderHeader, LineNo, LineRange, NonEmpty, Oid, ProtocolVersion, RefSpec, RenderChunk,
    RenderContent, RenderOpts, RenderTarget, RepoId, RepoPath, Request, RequestId, ResolvedRef,
    ResolvedSource, ResolvedTarget, Response, Review, ReviewId, ReviewSnapshot, ReviewStatus,
    ReviewTarget, Row, SchemaVersion, Seq, ServerMsg, Side, StreamItem, Timestamp, TreeEntry,
    TreeEntryKind, TreeOid, TreeSnapshot, WorkspaceId,
};

fn repo_id() -> RepoId {
    RepoId::from_parts(2, 2)
}

fn review_id() -> ReviewId {
    ReviewId::from_parts(4, 1)
}

fn path(p: &str) -> RepoPath {
    RepoPath::new(p).unwrap()
}

fn blob(fill: u8) -> nits_protocol::BlobOid {
    nits_protocol::BlobOid::new(Oid::from_bytes([fill; 20]))
}

fn other() -> Author {
    Author::Human {
        name: "ada".into(),
        machine: "workstation".into(),
    }
}

fn review() -> Review {
    Review {
        id: review_id(),
        workspace_id: WorkspaceId::from_parts(3, 3),
        title: "Add the parser".into(),
        targets: NonEmpty::singleton(ReviewTarget {
            repo_id: repo_id(),
            base: RefSpec::Branch {
                name: "main".into(),
            },
            head: RefSpec::WorkingTree,
        }),
        created: Timestamp::from_millis(1_000),
        status: ReviewStatus::Open,
    }
}

fn snapshot() -> ReviewSnapshot {
    let resolved = |fill: u8, source: ResolvedSource| ResolvedRef {
        tree: TreeOid::from_bytes([fill; 20]),
        source,
    };
    ReviewSnapshot {
        review: review(),
        resolved: Some(NonEmpty::singleton(ResolvedTarget {
            repo_id: repo_id(),
            base: resolved(
                1,
                ResolvedSource::Commit {
                    oid: nits_protocol::CommitOid::from_bytes([1; 20]),
                },
            ),
            head: resolved(
                2,
                ResolvedSource::WorkingTree {
                    dirty: Vec::new(),
                    branch: None,
                },
            ),
        })),
        threads: Vec::new(),
        comments: Vec::new(),
        viewed: Vec::new(),
        requests: Vec::new(),
        seq: Seq::new(1),
        checkpoints: Vec::new(),
    }
}

fn header(p: &str, change: ChangeKind, chunk_count: u32) -> FileRenderHeader {
    FileRenderHeader {
        repo_id: repo_id(),
        path: path(p),
        target: RenderTarget::Diff { change },
        opts: RenderOpts::default(),
        lang: Some("rust".into()),
        content: RenderContent::Text {
            total_rows: 8 * chunk_count,
            chunk_rows: 8,
            chunk_count,
            highlighted: true,
            additions: 2,
            deletions: 1,
            gaps: nits_protocol::GapTable::default(),
        },
    }
}

/// Eight rows: a hunk header, context, one removed/added pair, context.
fn chunk(index: u32) -> RenderChunk {
    use nits_protocol::Cell;
    let cell = |n: u32, text: &str| Cell {
        line_no: LineNo::new(n).unwrap(),
        text: text.into(),
        spans: Vec::new(),
        changed: Vec::new(),
    };
    let base = index * 8;
    RenderChunk {
        index: ChunkIndex::new(index),
        rows: vec![
            Row::HunkHeader {
                text: format!("@@ -{},7 +{},8 @@", base + 1, base + 1),
            },
            Row::Context {
                left: cell(base + 1, "fn parse() {"),
                right: cell(base + 1, "fn parse() {"),
            },
            Row::Removed {
                left: cell(base + 2, "    old()"),
            },
            Row::Added {
                right: cell(base + 2, "    new()"),
            },
            Row::Added {
                right: cell(base + 3, "    newer()"),
            },
            Row::Context {
                left: cell(base + 3, "}"),
                right: cell(base + 4, "}"),
            },
            Row::Context {
                left: cell(base + 4, ""),
                right: cell(base + 5, ""),
            },
            Row::Context {
                left: cell(base + 5, "// end"),
                right: cell(base + 6, "// end"),
            },
        ],
    }
}

fn comment(n: u128, anchor: Anchor, body: &str) -> Comment {
    let id = CommentId::from_parts(2_000 + u64::try_from(n).unwrap(), n);
    Comment {
        id,
        review_id: review_id(),
        thread_id: nits_client_core::thread_id_of(id),
        author: other(),
        kind: CommentKind::Note,
        anchor,
        body: body.into(),
        created: Timestamp::from_millis(2_000 + i64::try_from(n).unwrap()),
        edited: None,
        state: CommentState::Live,
        context: None,
    }
}

fn request_id(effects: &[Effect]) -> RequestId {
    effects
        .iter()
        .find_map(|e| match e {
            Effect::Send(ClientMsg::Request { id, .. }) => Some(*id),
            Effect::Send(ClientMsg::Hello { .. } | ClientMsg::Cancel { .. })
            | Effect::Connect
            | Effect::Disconnect
            | Effect::Render(_)
            | Effect::Persist { .. }
            | Effect::Load { .. }
            | Effect::Remove { .. } => None,
        })
        .unwrap()
}

/// Connect, subscribe, learn the review, open it over a stream.
fn opened() -> ClientCore {
    let mut core = ClientCore::new(Config {
        client_id: ClientId::from_parts(1, 1),
        client: BuildInfo {
            name: "snapshot".into(),
            version: "0".into(),
        },
        author: Author::Human {
            name: "someone".into(),
            machine: "laptop".into(),
        },
        id_seed: IdSeed(7),
        cache: CacheConfig::default(),
    });
    core.handle(Input::Tick(5_000)).unwrap();
    core.handle(Input::User(Action::Connect)).unwrap();
    core.handle(Input::Transport(TransportEvent::Connected))
        .unwrap();
    let effects = core
        .handle(Input::Server(ServerMsg::Welcome {
            protocol: ProtocolVersion::CURRENT,
            daemon: BuildInfo {
                name: "nitsd".into(),
                version: "0".into(),
            },
            schema: SchemaVersion::CURRENT,
            upgrade: None,
        }))
        .unwrap();
    core.handle(Input::Server(ServerMsg::Response {
        id: request_id(&effects),
        response: Response::Subscribed { seq: Seq::new(1) },
    }))
    .unwrap();
    core.handle(Input::Server(ServerMsg::Event {
        event: Event {
            seq: Seq::new(2),
            ts: Timestamp::from_millis(1_000),
            author: other(),
            client_id: ClientId::from_parts(9, 9),
            client_seq: ClientSeq::new(1),
            body: EventBody::ReviewCreated { review: review() },
        },
    }))
    .unwrap();
    let effects = core
        .handle(Input::User(Action::OpenReview {
            review_id: review_id(),
        }))
        .unwrap();
    let id = request_id(&effects);
    let mut item = |item: StreamItem| {
        core.handle(Input::Server(ServerMsg::StreamItem { id, item }))
            .unwrap();
    };
    item(StreamItem::ReviewSnapshot {
        snapshot: snapshot(),
    });
    let entry = |p: &str, fill: u8| TreeEntry {
        path: path(p),
        kind: TreeEntryKind::File {
            oid: blob(fill),
            size: 10,
            executable: false,
        },
    };
    item(StreamItem::TreeSnapshot {
        snapshot: TreeSnapshot {
            repo_id: repo_id(),
            root_oid: TreeOid::from_bytes([1; 20]),
            entries: vec![entry("README.md", 1), entry("src/lib.rs", 10)],
        },
    });
    item(StreamItem::TreeSnapshot {
        snapshot: TreeSnapshot {
            repo_id: repo_id(),
            root_oid: TreeOid::from_bytes([2; 20]),
            entries: vec![
                entry("README.md", 1),
                entry("src/lib.rs", 11),
                entry("src/parser.rs", 12),
            ],
        },
    });
    let files = [
        (
            "src/lib.rs",
            ChangeKind::Modified {
                old: blob(10),
                new: blob(11),
            },
            2,
        ),
        ("src/parser.rs", ChangeKind::Added { new: blob(12) }, 1),
    ];
    for (p, change, chunks) in files {
        item(StreamItem::Header {
            header: header(p, change, chunks),
        });
        item(StreamItem::Chunk {
            repo_id: repo_id(),
            path: path(p),
            chunk: chunk(0),
        });
    }
    core.handle(Input::Server(ServerMsg::StreamEnd { id }))
        .unwrap();
    // Two comments from someone else: one on the added line, one review-wide.
    let comments = [
        comment(
            1,
            Anchor::Lines {
                repo_id: repo_id(),
                path: path("src/lib.rs"),
                side: Side::Head,
                blob_oid: blob(11),
                lines: LineRange::single(LineNo::new(2).unwrap()),
                context_hash: ContextHash::new(1),
            },
            "Is `new` the right name?",
        ),
        comment(2, Anchor::Review, "Looks good overall."),
    ];
    for (i, c) in comments.into_iter().enumerate() {
        let seq = 3 + u64::try_from(i).unwrap();
        core.handle(Input::Server(ServerMsg::Event {
            event: Event {
                seq: Seq::new(seq),
                ts: Timestamp::from_millis(2_000),
                author: other(),
                client_id: ClientId::from_parts(9, 9),
                client_seq: ClientSeq::new(1),
                body: EventBody::CommentCreated { comment: c },
            },
        }))
        .unwrap();
    }
    let _ = Request::ListWorkspaces;
    core
}

fn settings() -> insta::Settings {
    let mut s = insta::Settings::clone_current();
    s.set_sort_maps(true);
    s
}

fn install_commits(core: &mut ClientCore) -> CommitOid {
    let effects = core
        .handle(Input::User(Action::ListCommits { repo_id: repo_id() }))
        .unwrap();
    let id = request_id(&effects);
    let sig = nits_protocol::Sig {
        name: "ada".into(),
        email: "ada@example.com".into(),
        time: Timestamp::from_millis(900),
        offset_minutes: 0,
    };
    let commits = [(2, "Finish parser"), (1, "Start parser")]
        .into_iter()
        .map(|(fill, subject)| CommitInfo {
            oid: CommitOid::from_bytes([fill; 20]),
            parents: Vec::new(),
            tree: TreeOid::from_bytes([fill; 20]),
            author: sig.clone(),
            committer: sig.clone(),
            subject: subject.into(),
            body: String::new(),
        })
        .collect();
    core.handle(Input::Server(ServerMsg::Response {
        id,
        response: Response::Commits { commits },
    }))
    .unwrap();
    CommitOid::from_bytes([2; 20])
}

#[test]
fn scope_picker_all_changes() {
    let mut core = opened();
    install_commits(&mut core);
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::CommitStepper { index: 0 },
    }))
    .unwrap();
    settings().bind(|| {
        insta::assert_json_snapshot!(
            "scope_picker_all_changes",
            serde_json::json!({
                "scope": core.view().scope,
                "focus": core.view().focus,
                "stepper": core.view().stepper,
            })
        );
    });
}

#[test]
fn scope_picker_commit() {
    let mut core = opened();
    let oid = install_commits(&mut core);
    core.handle(Input::User(Action::SetScope {
        scope: nits_client_core::ScopeChoice::Commit {
            repo_id: repo_id(),
            oid,
        },
    }))
    .unwrap();
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::CommitStepper { index: 1 },
    }))
    .unwrap();
    settings().bind(|| {
        insta::assert_json_snapshot!(
            "scope_picker_commit",
            serde_json::json!({
                "scope": core.view().scope,
                "focus": core.view().focus,
                "stepper": core.view().stepper,
            })
        );
    });
}

#[test]
fn sidebar_visibility_states() {
    let mut core = opened();
    settings().bind(|| {
        insta::assert_json_snapshot!(
            "sidebar_visible",
            serde_json::json!({
                "prefs": core.view().prefs,
                "focus": core.view().focus,
            })
        );
    });
    core.handle(Input::User(Action::ToggleSidebar)).unwrap();
    settings().bind(|| {
        insta::assert_json_snapshot!(
            "sidebar_hidden",
            serde_json::json!({
                "prefs": core.view().prefs,
                "focus": core.view().focus,
            })
        );
    });
}

#[test]
fn view_after_open() {
    let core = opened();
    settings().bind(|| insta::assert_json_snapshot!("after_open", core.view()));
}

#[test]
fn view_with_file_open_and_comments_placed() {
    let mut core = opened();
    // The open auto-opened the first file (src/lib.rs) and focused the
    // diff; land on the commented row.
    for chord in ["] c"] {
        let seq: nits_client_core::KeySeq = chord.parse().unwrap();
        for c in seq.chords() {
            core.handle(Input::Key(*c)).unwrap();
        }
    }
    assert!(matches!(core.view().focus, Focus::Diff { .. }));
    settings().bind(|| insta::assert_json_snapshot!("file_open", core.view()));
}

#[test]
fn view_with_pending_reply_and_help() {
    let mut core = opened();
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Thread { index: 0 },
    }))
    .unwrap();
    core.handle(Input::Key(KeyChord::char('r'))).unwrap();
    core.handle(Input::User(Action::DraftSubmitted {
        body: "Renamed in the next commit.".into(),
    }))
    .unwrap();
    core.handle(Input::Key(KeyChord::char('?'))).unwrap();
    settings().bind(|| insta::assert_json_snapshot!("pending_reply_and_help", core.view()));
}
