//! Plan 3.5 keyboard model: every action is reachable from a key, the
//! table has no conflicts, sequences resolve and expire, `?` always helps,
//! and focus drives the viewport.

// Scenario tests and their fixture read top to bottom.
#![allow(clippy::too_many_lines)]

use std::collections::BTreeSet;

use nits_client_core::{
    Action, ActionKind, CacheConfig, ClientCore, Command, Config, Context, CoreError, Effect,
    Focus, IdSeed, Input, KeyChord, Keymap, LastKey, NamedKey, NoTarget, SEQ_TIMEOUT_MS,
    TransportEvent,
};
use nits_protocol::{
    Anchor, Author, BuildInfo, ChangeKind, ChunkIndex, ClientId, ClientMsg, Comment, CommentId,
    CommentKind, CommentState, ExpandDir, FileChange, FileRenderHeader, Gap, NonEmpty, Oid,
    ProtocolVersion, RefSpec, RenderChunk, RenderContent, RenderOpts, RenderTarget, RepoId,
    RepoPath, Request, Review, ReviewId, ReviewSnapshot, ReviewStatus, ReviewTarget, Row,
    SchemaVersion, Seq, ServerMsg, Side, StreamItem, Timestamp, TreeEntry, TreeEntryKind, TreeOid,
    TreeSnapshot, ViewSection, WorkspaceId,
};
use strum::IntoEnumIterator;

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

fn review() -> Review {
    Review {
        id: review_id(),
        workspace_id: WorkspaceId::from_parts(3, 3),
        title: "a review".into(),
        targets: NonEmpty::singleton(ReviewTarget {
            repo_id: repo_id(),
            base: RefSpec::Branch {
                name: "main".into(),
            },
            head: RefSpec::Head,
        }),
        created: Timestamp::from_millis(0),
        status: ReviewStatus::Open,
    }
}

fn comment(n: u128, anchor: Anchor) -> Comment {
    let id = CommentId::from_parts(5, n);
    Comment {
        id,
        review_id: review_id(),
        thread_id: nits_client_core::thread_id_of(id),
        author: Author::Human {
            name: "other".into(),
            machine: "host".into(),
        },
        kind: CommentKind::Note,
        anchor,
        body: format!("c{n}"),
        created: Timestamp::from_millis(i64::try_from(n).unwrap()),
        edited: None,
        state: CommentState::Live,
        context: None,
    }
}

/// Snapshot with one thread over lines 3–5 (head) of a.rs, one on the
/// removed side of line 5 of b.rs (a file that is not open), and one
/// review-level.
fn snapshot() -> ReviewSnapshot {
    use nits_protocol::{ContextHash, LineNo, LineRange, Side};
    let c1 = comment(
        1,
        Anchor::Lines {
            repo_id: repo_id(),
            path: path("src/a.rs"),
            side: Side::Head,
            blob_oid: blob(11),
            // A range, so the rows it covers and the row its card hangs
            // under are different things.
            lines: LineRange::new(LineNo::new(3).unwrap(), LineNo::new(5).unwrap()).unwrap(),
            context_hash: ContextHash::new(0),
        },
    );
    let c2 = comment(2, Anchor::Review);
    let c3 = comment(
        3,
        Anchor::Lines {
            repo_id: repo_id(),
            path: path("b.rs"),
            side: Side::Base,
            blob_oid: blob(10),
            lines: LineRange::single(LineNo::new(5).unwrap()),
            context_hash: ContextHash::new(0),
        },
    );
    let mut s = ReviewSnapshot {
        review: review(),
        resolved: Some(NonEmpty::singleton(nits_protocol::ResolvedTarget {
            repo_id: repo_id(),
            base: nits_protocol::ResolvedRef {
                tree: TreeOid::from_bytes([1; 20]),
                source: nits_protocol::ResolvedSource::Commit {
                    oid: nits_protocol::CommitOid::from_bytes([1; 20]),
                },
            },
            head: nits_protocol::ResolvedRef {
                tree: TreeOid::from_bytes([2; 20]),
                source: nits_protocol::ResolvedSource::Commit {
                    oid: nits_protocol::CommitOid::from_bytes([2; 20]),
                },
            },
        })),
        threads: Vec::new(),
        comments: Vec::new(),
        viewed: Vec::new(),
        requests: Vec::new(),
        seq: Seq::new(1),
    };
    let meta = nits_client_core::EventMeta {
        author: c1.author.clone(),
        ts: Timestamp::from_millis(0),
    };
    for c in [c1, c2, c3] {
        nits_client_core::apply_body(
            &mut s,
            &meta,
            &nits_protocol::EventBody::CommentCreated { comment: c },
        );
    }
    s
}

fn header(p: &str) -> FileRenderHeader {
    FileRenderHeader {
        repo_id: repo_id(),
        path: path(p),
        target: RenderTarget::Diff {
            change: ChangeKind::Modified {
                old: blob(10),
                new: blob(11),
            },
        },
        opts: RenderOpts::default(),
        lang: None,
        content: RenderContent::Text {
            total_rows: 300,
            chunk_rows: 100,
            chunk_count: 3,
            highlighted: false,
            additions: 1,
            deletions: 1,
            // The gap table the daemon carries on the header, so a chord
            // can name a gap whose chunk is not cached.
            gaps: nits_protocol::GapTable::try_from(vec![nits_protocol::GapRow {
                gap: Gap::new(1),
                row: EXPANDER_ROW,
            }])
            .unwrap(),
        },
    }
}

fn chunk(index: u32) -> RenderChunk {
    use nits_protocol::{Cell, LineNo};
    let cell = |n: u32| Cell {
        line_no: LineNo::new(n).unwrap(),
        text: format!("l{n}"),
        spans: Vec::new(),
        changed: Vec::new(),
    };
    RenderChunk {
        index: ChunkIndex::new(index),
        rows: (0..100)
            .map(|i| {
                let n = index * 100 + i + 1;
                // One of every commentable row shape, so the side-aware
                // paths have something to aim at (row 4 modified, 6 added,
                // 8 removed), and a hidden run between the two hunks for
                // the expander paths.
                match n {
                    1 | 150 => Row::HunkHeader {
                        text: format!("@@ {n} @@"),
                    },
                    5 => Row::Modified {
                        left: cell(n),
                        right: cell(n),
                    },
                    7 => Row::Added { right: cell(n) },
                    9 => Row::Removed { left: cell(n) },
                    149 => Row::Expander {
                        hidden: 40,
                        dir: ExpandDir::Both,
                        gap: Gap::new(1),
                    },
                    // A deliberately asymmetric split row used to prove
                    // realignment preserves the focused side, not merely
                    // whichever side the host happens to inspect first.
                    171 => Row::Modified {
                        left: cell(500),
                        right: cell(n),
                    },
                    _ => Row::Context {
                        left: cell(n),
                        right: cell(n),
                    },
                }
            })
            .collect(),
    }
}

fn requests(effects: &[Effect]) -> Vec<Request> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Send(ClientMsg::Request { request, .. }) => Some(request.clone()),
            Effect::Send(ClientMsg::Hello { .. } | ClientMsg::Cancel { .. })
            | Effect::Connect
            | Effect::Disconnect
            | Effect::Render(_)
            | Effect::Persist { .. }
            | Effect::Load { .. }
            | Effect::Remove { .. } => None,
        })
        .collect()
}

fn rendered(effects: &[Effect]) -> Vec<ViewSection> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Render(d) => Some(d.sections.clone()),
            Effect::Send(_)
            | Effect::Connect
            | Effect::Disconnect
            | Effect::Load { .. }
            | Effect::Persist { .. }
            | Effect::Remove { .. } => None,
        })
        .flatten()
        .collect()
}

/// A subscribed core with the review open, two files (src/a.rs, b.rs),
/// head tree cached, every chunk of a.rs cached, the file list known.
fn ready() -> ClientCore {
    let mut core = ClientCore::new(Config {
        client_id: ClientId::from_parts(1, 1),
        client: BuildInfo {
            name: "test".into(),
            version: "0".into(),
        },
        author: Author::Human {
            name: "someone".into(),
            machine: "host".into(),
        },
        id_seed: IdSeed(7),
        cache: CacheConfig::default(),
    });
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
    let id = effects
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
        .unwrap();
    core.handle(Input::Server(ServerMsg::Response {
        id,
        response: nits_protocol::Response::Subscribed { seq: Seq::new(1) },
    }))
    .unwrap();
    core.handle(Input::Server(ServerMsg::Event {
        event: nits_protocol::Event {
            seq: Seq::new(2),
            ts: Timestamp::from_millis(0),
            author: Author::Human {
                name: "other".into(),
                machine: "host".into(),
            },
            client_id: ClientId::from_parts(9, 9),
            client_seq: nits_protocol::ClientSeq::new(1),
            body: nits_protocol::EventBody::ReviewCreated { review: review() },
        },
    }))
    .unwrap();
    let effects = core
        .handle(Input::User(Action::OpenReview {
            review_id: review_id(),
        }))
        .unwrap();
    let id = effects
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
        .unwrap();
    let item = |core: &mut ClientCore, item: StreamItem| {
        core.handle(Input::Server(ServerMsg::StreamItem { id, item }))
            .unwrap();
    };
    item(
        &mut core,
        StreamItem::ReviewSnapshot {
            snapshot: snapshot(),
        },
    );
    let entry = |p: &str| TreeEntry {
        path: path(p),
        kind: TreeEntryKind::File {
            oid: blob(11),
            size: 1,
            executable: false,
        },
    };
    item(
        &mut core,
        StreamItem::TreeSnapshot {
            snapshot: TreeSnapshot {
                repo_id: repo_id(),
                root_oid: TreeOid::from_bytes([2; 20]),
                entries: vec![entry("b.rs"), entry("src/a.rs")],
            },
        },
    );
    for p in ["src/a.rs", "b.rs"] {
        item(&mut core, StreamItem::Header { header: header(p) });
    }
    for c in 0..3 {
        item(
            &mut core,
            StreamItem::Chunk {
                repo_id: repo_id(),
                path: path("src/a.rs"),
                chunk: chunk(c),
            },
        );
    }
    core.handle(Input::Server(ServerMsg::StreamEnd { id }))
        .unwrap();
    let _ = FileChange {
        repo_id: repo_id(),
        path: path("x"),
        kind: ChangeKind::Added { new: blob(1) },
    };
    core
}

fn press(core: &mut ClientCore, keys: &str) -> Result<Vec<Effect>, CoreError> {
    let seq: nits_client_core::KeySeq = keys.parse().unwrap();
    let mut out = Vec::new();
    for c in seq.chords() {
        out.extend(core.handle(Input::Key(*c))?);
    }
    Ok(out)
}

#[test]
fn every_action_is_reachable_from_a_binding() {
    // Actions whose payload only the host can supply (text, a workspace
    // choice, a scroll position — though `Open` also produces Viewport).
    let host_only: BTreeSet<ActionKind> = [
        ActionKind::DraftSubmitted,
        ActionKind::Reply,        // `r` opens a reply draft; the text is host's
        ActionKind::EditComment,  // edit text is host's
        ActionKind::CreateReview, // title and targets come from a form
        ActionKind::ListReviews,
        ActionKind::FileSearch,   // produced (with an empty query) — see below
        ActionKind::SetBrowseRef, // the ref text comes from the Browse picker
        ActionKind::RunCommand,   // the actions palette picks the command
        ActionKind::CommentLines, // mouse drag across lines
        ActionKind::CommentFile,  // the file header's comment button
        ActionKind::SearchFirst,  // search input enters the first result
        ActionKind::SearchStep,   // search result navigation
        ActionKind::OpenSearchResult, // search Enter activates current core selection
        ActionKind::OpenRefSelector, // header buttons own target selection
        ActionKind::RefSelectorQuery, // selector text is host-owned
        ActionKind::RefSelectorStep, // selector input forwards motion
        ActionKind::SelectRef,    // selector clicks an explicit option
        ActionKind::SelectCurrentRef, // selector Enter activates core state
        ActionKind::CloseRefSelector, // selector Escape
        ActionKind::Viewport,     // the host reports what it shows; keys
                                  // open a file with `OpenFileAt`, which knows the row and side
    ]
    .into_iter()
    .collect();
    let mut reached: BTreeSet<ActionKind> = BTreeSet::new();
    let map = Keymap::default_table();
    // Every command has a binding somewhere.
    for cmd in Command::iter() {
        assert!(
            map.bindings().iter().any(|b| b.command == cmd),
            "{cmd} is not bound"
        );
    }
    // Drive the core through states where each command means something.
    let mut core = ready();
    let record = |core: &mut ClientCore, keys: &str| {
        if let Ok(effects) = press(core, keys) {
            let _ = effects;
        }
    };
    // Tree focus.
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Tree { index: 0 },
    }))
    .unwrap();
    for keys in [
        "enter", "j", "k", "G", "g g", "v", "v", "c", "] f", "[ f", "ctrl+p", "esc",
    ] {
        record(&mut core, keys);
    }
    // Diff focus (after `enter` on a file).
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Tree { index: 0 },
    }))
    .unwrap();
    // The record loop's `enter` collapsed the (default-open) root;
    // reopen it, then walk: root, src (a.rs), b.rs.
    press(&mut core, "enter").unwrap();
    press(&mut core, "j").unwrap(); // src dir
    press(&mut core, "j").unwrap(); // src/a.rs
    let names: Vec<String> = nits_client_core::visible_nodes(core.view())
        .iter()
        .map(|n| match n {
            nits_client_core::TreeNode::Dir { name, .. }
            | nits_client_core::TreeNode::File { name, .. } => name.clone(),
        })
        .collect();
    assert_eq!(names[1..], ["src", "a.rs", "b.rs"]);
    press(&mut core, "enter").unwrap(); // open
    assert!(matches!(core.view().focus, Focus::Diff { .. }));
    for keys in [
        "j", "k", "ctrl+d", "ctrl+u", "G", "g g", "n", "p", "] c", "[ c", "] f", "[ f", "s", "v",
        "tab",
    ] {
        record(&mut core, keys);
    }
    // Comment from a diff row, then discard.
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Diff {
            row: 4,
            side: Side::Head,
        },
    }))
    .unwrap();
    press(&mut core, "c").unwrap();
    assert_eq!(core.view().focus, Focus::Composer);
    press(&mut core, "esc").unwrap();
    // Thread focus: open, reply, resolve.
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Thread { index: 0 },
    }))
    .unwrap();
    for keys in ["enter", "x", "x", "r", "esc", "j", "k", "?", "?", "esc"] {
        record(&mut core, keys);
    }
    // Whitespace toggle re-keys every render (file list refetched), so last.
    press(&mut core, "w").unwrap();
    press(&mut core, "ctrl+shift+d").ok();
    press(&mut core, "ctrl+shift+c").ok();
    // Collect what every command resolves to in a few representative
    // states rather than what happened to run: resolution is the contract.
    let mut states: Vec<ClientCore> = Vec::new();
    let base = ready();
    for focus in [
        Focus::ReviewList { index: 0 },
        Focus::Tree { index: 0 },
        Focus::Thread { index: 0 },
        Focus::Help,
    ] {
        let mut c = ready();
        let _ = c.handle(Input::User(Action::SetFocus { focus }));
        states.push(c);
    }
    let mut with_file = ready();
    with_file
        .handle(Input::User(Action::Viewport {
            file: nits_client_core::FileRef {
                repo_id: repo_id(),
                path: path("src/a.rs"),
            },
            first_row: 0,
            last_row: 59,
        }))
        .unwrap();
    with_file
        .handle(Input::User(Action::SetFocus {
            focus: Focus::Diff {
                row: 4,
                side: Side::Head,
            },
        }))
        .unwrap();
    states.push(with_file);
    // On an expander row: `enter` and `z u`/`z d` open that hidden run.
    states.push(on_row(EXPANDER_ROW, Side::Head));
    // Visual mode on: `esc` (Back) and `V` resolve to LeaveVisual.
    let mut with_visual = ready();
    with_visual
        .handle(Input::User(Action::Viewport {
            file: nits_client_core::FileRef {
                repo_id: repo_id(),
                path: path("src/a.rs"),
            },
            first_row: 0,
            last_row: 59,
        }))
        .unwrap();
    with_visual
        .handle(Input::User(Action::SetFocus {
            focus: Focus::Diff {
                row: 4,
                side: Side::Head,
            },
        }))
        .unwrap();
    press(&mut with_visual, "V").unwrap();
    states.push(with_visual);
    let mut with_draft = ready();
    with_draft
        .handle(Input::User(Action::DraftOpened {
            anchor: Anchor::Review,
        }))
        .unwrap();
    states.push(with_draft);
    let mut with_stepper = ready();
    let effects = with_stepper
        .handle(Input::User(Action::ListCommits { repo_id: repo_id() }))
        .unwrap();
    let id = effects
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
        .unwrap();
    let sig = nits_protocol::Sig {
        name: "ada".into(),
        email: "ada@example.com".into(),
        time: Timestamp::from_millis(5),
        offset_minutes: 0,
    };
    with_stepper
        .handle(Input::Server(ServerMsg::Response {
            id,
            response: nits_protocol::Response::Commits {
                commits: [2u8, 1u8]
                    .into_iter()
                    .map(|fill| nits_protocol::CommitInfo {
                        oid: nits_protocol::CommitOid::from_bytes([fill; 20]),
                        parents: Vec::new(),
                        tree: TreeOid::from_bytes([fill; 20]),
                        author: sig.clone(),
                        committer: sig.clone(),
                        subject: format!("commit {fill}"),
                        body: String::new(),
                    })
                    .collect(),
            },
        }))
        .unwrap();
    with_stepper
        .handle(Input::User(Action::SetScope {
            scope: nits_client_core::ScopeChoice::Commit {
                repo_id: repo_id(),
                oid: nits_protocol::CommitOid::from_bytes([2; 20]),
            },
        }))
        .unwrap();
    with_stepper
        .handle(Input::User(Action::SetFocus {
            focus: Focus::CommitStepper { index: 1 },
        }))
        .unwrap();
    states.push(with_stepper);
    // A resolved thread of our own, and a viewed file: the toggles' other
    // halves.
    let mut toggled = ready();
    let mine = Author::Human {
        name: "someone".into(),
        machine: "host".into(),
    };
    let own = Comment {
        author: mine.clone(),
        ..comment(9, Anchor::Review)
    };
    let thread_id = own.thread_id;
    let bodies = [
        nits_protocol::EventBody::CommentCreated { comment: own },
        nits_protocol::EventBody::ThreadResolved {
            review_id: review_id(),
            thread_id,
        },
        nits_protocol::EventBody::FileViewed {
            review_id: review_id(),
            repo_id: repo_id(),
            path: path("src/a.rs"),
            viewer: mine.as_human().unwrap(),
            blob_oid: Some(blob(11)),
        },
    ];
    for (i, body) in bodies.into_iter().enumerate() {
        let seq = 3 + u64::try_from(i).unwrap();
        toggled
            .handle(Input::Server(ServerMsg::Event {
                event: nits_protocol::Event {
                    seq: Seq::new(seq),
                    ts: Timestamp::from_millis(0),
                    author: mine.clone(),
                    client_id: ClientId::from_parts(9, 9),
                    client_seq: nits_protocol::ClientSeq::new(1),
                    body,
                },
            }))
            .unwrap();
    }
    let index = toggled
        .view()
        .threads
        .iter()
        .position(|t| t.id == thread_id)
        .unwrap();
    toggled
        .handle(Input::User(Action::SetFocus {
            focus: Focus::Thread { index },
        }))
        .unwrap();
    states.push(toggled);
    // A suggestion thread, focused: `a` applies it.
    let mut with_suggestion = ready();
    let suggestion = Comment {
        kind: CommentKind::Suggestion {
            patch: "@@ -1 +1 @@\n-a\n+b\n".into(),
        },
        ..comment(11, Anchor::Review)
    };
    let suggestion_thread = suggestion.thread_id;
    with_suggestion
        .handle(Input::Server(ServerMsg::Event {
            event: nits_protocol::Event {
                seq: Seq::new(3),
                ts: Timestamp::from_millis(0),
                author: Author::Human {
                    name: "other".into(),
                    machine: "host".into(),
                },
                client_id: ClientId::from_parts(9, 9),
                client_seq: nits_protocol::ClientSeq::new(1),
                body: nits_protocol::EventBody::CommentCreated {
                    comment: suggestion,
                },
            },
        }))
        .unwrap();
    let index = with_suggestion
        .view()
        .threads
        .iter()
        .position(|t| t.id == suggestion_thread)
        .unwrap();
    with_suggestion
        .handle(Input::User(Action::SetFocus {
            focus: Focus::Thread { index },
        }))
        .unwrap();
    assert!(with_suggestion.view().threads[index].suggestion);
    states.push(with_suggestion);
    let mut viewed = ready();
    viewed
        .handle(Input::User(Action::Viewport {
            file: nits_client_core::FileRef {
                repo_id: repo_id(),
                path: path("src/a.rs"),
            },
            first_row: 0,
            last_row: 59,
        }))
        .unwrap();
    viewed
        .handle(Input::User(Action::MarkViewed {
            file: nits_client_core::FileRef {
                repo_id: repo_id(),
                path: path("src/a.rs"),
            },
        }))
        .unwrap();
    states.push(viewed);
    let mut with_search = ready();
    with_search
        .handle(Input::User(Action::FileSearch {
            query: Some("a".into()),
        }))
        .unwrap();
    states.push(with_search);
    // A thread whose root is outdated with a recorded context: `enter`
    // jumps to the original diff (OpenOriginalDiff).
    let mut with_outdated = ready();
    let anchor = Anchor::Lines {
        repo_id: repo_id(),
        path: path("src/a.rs"),
        side: nits_protocol::Side::Head,
        blob_oid: blob(4),
        lines: nits_protocol::LineRange::single(nits_protocol::LineNo::new(2).unwrap()),
        context_hash: nits_protocol::ContextHash::new(0),
    };
    let outdated = nits_protocol::Comment {
        state: CommentState::Outdated {
            last_good_anchor: anchor.clone(),
        },
        context: Some(ChangeKind::Modified {
            old: blob(3),
            new: blob(4),
        }),
        anchor,
        ..comment(12, Anchor::Review)
    };
    let outdated_thread = outdated.thread_id;
    with_outdated
        .handle(Input::Server(ServerMsg::Event {
            event: nits_protocol::Event {
                seq: Seq::new(3),
                ts: Timestamp::from_millis(0),
                author: Author::Human {
                    name: "other".into(),
                    machine: "host".into(),
                },
                client_id: ClientId::from_parts(9, 9),
                client_seq: nits_protocol::ClientSeq::new(1),
                body: nits_protocol::EventBody::CommentCreated { comment: outdated },
            },
        }))
        .unwrap();
    let index = with_outdated
        .view()
        .threads
        .iter()
        .position(|t| t.id == outdated_thread)
        .unwrap();
    with_outdated
        .handle(Input::User(Action::SetFocus {
            focus: Focus::Thread { index },
        }))
        .unwrap();
    assert!(with_outdated.view().threads[index].outdated);
    states.push(with_outdated);
    states.push(base);
    for state in &states {
        for cmd in Command::iter() {
            if let Ok(action) = nits_client_core::resolve_command(state, cmd) {
                reached.insert(ActionKind::from(&action));
            }
        }
    }
    let all: BTreeSet<ActionKind> = ActionKind::iter().collect();
    let missing: Vec<ActionKind> = all
        .difference(&reached)
        .filter(|k| !host_only.contains(k))
        .copied()
        .collect();
    assert!(missing.is_empty(), "unreachable from any key: {missing:?}");
    // FileSearch is reachable too (Global ctrl+p) — listed above only so the
    // allowlist documents that its *query* is host text.
    assert!(reached.contains(&ActionKind::FileSearch));
}

#[test]
fn sequences_resolve_and_expire() {
    let mut core = ready();
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Tree { index: 0 },
    }))
    .unwrap();
    // Diffing mode auto-expands: root, src (with a.rs), b.rs are visible.
    press(&mut core, "G").unwrap();
    assert_eq!(core.view().focus, Focus::Tree { index: 3 });
    // `g` alone waits for the rest; the hint bar switches to the group.
    let effects = core.handle(Input::Key(KeyChord::char('g'))).unwrap();
    assert_eq!(rendered(&effects), vec![ViewSection::Hints]);
    assert_eq!(core.view().pending_keys, "g");
    assert!(!core.view().hints.is_empty());
    assert_eq!(core.pending_chords().len(), 1);
    core.handle(Input::Tick(100)).unwrap();
    let effects = core.handle(Input::Key(KeyChord::char('g'))).unwrap();
    assert_eq!(
        rendered(&effects),
        vec![ViewSection::Focus, ViewSection::Hints]
    );
    assert_eq!(core.view().pending_keys, "");
    assert_eq!(core.view().focus, Focus::Tree { index: 0 });
    assert!(core.pending_chords().is_empty());
    // A pending prefix never expires on the clock (vim-like): it waits.
    core.handle(Input::Key(KeyChord::char('g'))).unwrap();
    core.handle(Input::Tick(100 + SEQ_TIMEOUT_MS)).unwrap();
    assert_eq!(core.pending_chords().len(), 1);
    // A key outside the group (esc included) cancels the sequence
    // silently and re-renders the hints.
    let effects = core.handle(Input::Key(KeyChord::char('q'))).unwrap();
    assert!(core.pending_chords().is_empty());
    assert_eq!(rendered(&effects), vec![ViewSection::Hints]);
    // A single unbound key is still a typed error.
    assert_eq!(
        core.handle(Input::Key(KeyChord::char('q'))),
        Err(CoreError::UnboundKey("q".into()))
    );
    // At an edge, movement is a typed no-op.
    assert_eq!(
        core.handle(Input::Key(KeyChord::char('k'))),
        Err(CoreError::NoTarget(NoTarget::AtEdge))
    );
    // In the composer, unbound keys are text for the host: accepted, ignored.
    core.handle(Input::User(Action::DraftOpened {
        anchor: Anchor::Review,
    }))
    .unwrap();
    assert_eq!(core.view().focus, Focus::Composer);
    // Text for the host's editor, not a command: the only thing the core
    // has to say about it is that it counted the key (a host acting on a
    // key before the answer arrives correlates against that count).
    assert_eq!(
        rendered(&core.handle(Input::Key(KeyChord::char('j'))).unwrap()),
        vec![ViewSection::Hints]
    );
    // Esc discards and focus returns to the tree.
    press(&mut core, "esc").unwrap();
    assert!(core.view().draft.is_none());
    assert_eq!(core.view().focus, Focus::Tree { index: 0 });
}

#[test]
fn leader_b_toggles_the_persisted_sidebar_preference() {
    let mut core = ready();
    let focus = core.view().focus;

    for hidden in [true, false] {
        let effects = press(&mut core, "space b").unwrap();
        assert_eq!(core.view().prefs.sidebar_hidden, hidden);
        assert_eq!(core.view().focus, focus);
        assert!(requests(&effects).is_empty());

        let writes: Vec<_> = effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Persist { key, value } => Some((key, value)),
                Effect::Send(_)
                | Effect::Connect
                | Effect::Disconnect
                | Effect::Render(_)
                | Effect::Load { .. }
                | Effect::Remove { .. } => None,
            })
            .collect();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].0, nits_client_core::ViewPrefs::KEY);
        let stored: nits_client_core::ViewPrefs = serde_json::from_slice(writes[0].1).unwrap();
        assert_eq!(stored.sidebar_hidden, hidden);
    }
}

#[test]
fn pending_diff_expand_group_lists_every_continuation() {
    let mut core = ready();
    assert!(matches!(core.view().focus, Focus::Diff { .. }));
    core.handle(Input::Key(KeyChord::char('z'))).unwrap();
    assert_eq!(core.view().pending_keys, "z");
    assert_eq!(core.view().pending_label.as_deref(), Some("Expand/scroll"));
    assert_eq!(core.view().hints.len(), 6);
    for (keys, command) in [
        ("u", Command::ExpandUp),
        ("d", Command::ExpandDown),
        ("c", Command::CommentOnFile),
        ("z", Command::CenterView),
        ("t", Command::ViewTop),
        ("b", Command::ViewBottom),
    ] {
        assert!(
            core.view()
                .hints
                .iter()
                .any(|hint| hint.keys == keys && hint.command == command),
            "pending z group omitted {keys}"
        );
    }
}

#[test]
fn diff_focus_scrolls_the_viewport_and_navigates_hunks_and_comments() {
    let mut core = ready();
    core.handle(Input::User(Action::Viewport {
        file: nits_client_core::FileRef {
            repo_id: repo_id(),
            path: path("src/a.rs"),
        },
        first_row: 0,
        last_row: 59,
    }))
    .unwrap();
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Diff {
            row: 0,
            side: Side::Head,
        },
    }))
    .unwrap();
    // Page down twice: the second leaves the window, which follows.
    press(&mut core, "ctrl+d").unwrap();
    assert_eq!(
        core.view().focus,
        Focus::Diff {
            row: 60,
            side: Side::Head
        }
    );
    let f = core
        .view()
        .review
        .as_ref()
        .unwrap()
        .open_file
        .clone()
        .unwrap();
    assert!(f.first_row <= 60 && 60 <= f.last_row);
    // Every chunk is cached: no requests, just renders.
    let effects = press(&mut core, "ctrl+d").unwrap();
    assert!(requests(&effects).is_empty());
    assert_eq!(
        core.view().focus,
        Focus::Diff {
            row: 120,
            side: Side::Head
        }
    );
    // Next hunk from the top is the header at row 149; previous is row 0.
    press(&mut core, "g g").unwrap();
    press(&mut core, "n").unwrap();
    assert_eq!(
        core.view().focus,
        Focus::Diff {
            row: 149,
            side: Side::Head
        }
    );
    press(&mut core, "p").unwrap();
    assert_eq!(
        core.view().focus,
        Focus::Diff {
            row: 0,
            side: Side::Head
        }
    );
    // Next comment is the thread on line 5 (row 4); Enter focuses it.
    press(&mut core, "] c").unwrap();
    assert_eq!(
        core.view().focus,
        Focus::Diff {
            row: 4,
            side: Side::Head
        }
    );
    press(&mut core, "enter").unwrap();
    assert_eq!(core.view().focus, Focus::Thread { index: 0 });
    // Enter on the thread goes back to its row; Esc closes the file.
    press(&mut core, "enter").unwrap();
    assert!(matches!(core.view().focus, Focus::Diff { .. }));
    press(&mut core, "esc").unwrap();
    assert!(core.view().diff.is_none());
    assert_eq!(core.view().focus, Focus::Tree { index: 0 });
    // Esc again closes the review; focus lands on the review list.
    press(&mut core, "esc").unwrap();
    assert!(core.view().review.is_none());
    assert_eq!(core.view().focus, Focus::ReviewList { index: 0 });
}

#[test]
fn help_and_hints_follow_focus_and_help_is_never_empty() {
    let mut core = ready();
    // The auto-open focuses the diff; this test reads the tree's hints.
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Tree { index: 0 },
    }))
    .unwrap();
    for ctx in Context::iter() {
        assert!(
            !Keymap::default_table().help(ctx).groups[0]
                .entries
                .is_empty(),
            "{ctx}"
        );
    }
    let hints_tree = core.view().hints.clone();
    assert!(hints_tree.iter().any(|h| h.command == Command::Open));
    let effects = press(&mut core, "?").unwrap();
    assert!(rendered(&effects).contains(&ViewSection::Help));
    assert_eq!(core.view().focus, Focus::Help);
    let help = core.view().help.as_ref().unwrap();
    assert_eq!(
        help.groups[0].context,
        Context::Tree,
        "help describes the context focused when it opened"
    );
    assert!(help.conflicts.is_empty());
    press(&mut core, "?").unwrap();
    assert!(core.view().help.is_none());
    assert_eq!(core.view().focus, Focus::Tree { index: 0 });
    // Hints changed with the context and back.
    assert_eq!(core.view().hints, hints_tree);

    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Diff {
            row: 0,
            side: Side::Head,
        },
    }))
    .unwrap();
    press(&mut core, "?").unwrap();
    let diff_help = &core.view().help.as_ref().unwrap().groups[0];
    assert_eq!(diff_help.context, Context::Diff);
    for (keys, command) in [
        ("z u", Command::ExpandUp),
        ("z d", Command::ExpandDown),
        ("z c", Command::CommentOnFile),
        ("z z", Command::CenterView),
        ("z t", Command::ViewTop),
        ("z b", Command::ViewBottom),
    ] {
        assert!(
            diff_help
                .entries
                .iter()
                .any(|entry| entry.keys == keys && entry.command == command),
            "Diff help omitted {keys}"
        );
    }
    press(&mut core, "?").unwrap();
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Tree { index: 0 },
    }))
    .unwrap();

    // A keys config from the KV re-derives the hints: `open` rebinds to
    // `o` across normal mode.
    let mut normal = std::collections::BTreeMap::new();
    normal.insert("open".to_owned(), vec!["o".to_owned()]);
    let mut bindings = std::collections::BTreeMap::new();
    bindings.insert(nits_client_core::Mode::Normal, normal);
    let config = nits_client_core::KeysConfig {
        leader: None,
        bindings,
        groups: std::collections::BTreeMap::new(),
    };
    let effects = core
        .handle(Input::Stored {
            key: Keymap::KEY.into(),
            value: Some(serde_json::to_vec(&config).unwrap()),
        })
        .unwrap();
    assert_eq!(rendered(&effects), vec![ViewSection::Hints]);
    assert!(core.view().hints.iter().any(|h| h.keys == "o"));
    assert_eq!(
        core.handle(Input::Key(KeyChord::named(NamedKey::Enter))),
        Err(CoreError::UnboundKey("enter".into()))
    );
    // `o` on the root dir toggles it: diffing dirs default open, so it
    // collapses.
    press(&mut core, "o").unwrap();
    assert!(core.view().tree.roots.iter().all(|n| matches!(
        n,
        nits_client_core::TreeNode::Dir {
            expanded: false,
            ..
        }
    )));
}

#[test]
fn search_step_moves_the_highlighted_hit() {
    let mut core = ready();
    // File find: two hits for "rs"; Down steps, the selection clamps.
    core.handle(Input::User(Action::FileSearch {
        query: Some("rs".into()),
    }))
    .unwrap();
    let hits = core.view().tree.search.as_ref().unwrap().hits.len();
    assert!(hits >= 2, "expected 2+ hits, got {hits}");
    assert_eq!(core.view().tree.search.as_ref().unwrap().selected, 0);
    core.handle(Input::User(Action::SearchStep {
        search: nits_client_core::SearchKind::Files,
        delta: 1,
    }))
    .unwrap();
    assert_eq!(core.view().tree.search.as_ref().unwrap().selected, 1);
    core.handle(Input::User(Action::SearchStep {
        search: nits_client_core::SearchKind::Files,
        delta: 100,
    }))
    .unwrap();
    assert_eq!(core.view().tree.search.as_ref().unwrap().selected, hits - 1);
    core.handle(Input::User(Action::SearchStep {
        search: nits_client_core::SearchKind::Files,
        delta: -100,
    }))
    .unwrap();
    assert_eq!(core.view().tree.search.as_ref().unwrap().selected, 0);
    // A new query resets the selection.
    core.handle(Input::User(Action::SearchStep {
        search: nits_client_core::SearchKind::Files,
        delta: 1,
    }))
    .unwrap();
    core.handle(Input::User(Action::FileSearch {
        query: Some("a".into()),
    }))
    .unwrap();
    assert_eq!(core.view().tree.search.as_ref().unwrap().selected, 0);
    // No search open: stepping is rejected and changes nothing.
    core.handle(Input::User(Action::FileSearch { query: None }))
        .unwrap();
    assert!(
        core.handle(Input::User(Action::SearchStep {
            search: nits_client_core::SearchKind::Files,
            delta: 1
        }))
        .is_err()
    );
}

#[test]
fn visual_mode_extends_a_selection_and_comments_on_it() {
    use nits_client_core::{Mode, VisualView};
    let mut core = ready();
    core.handle(Input::User(Action::Viewport {
        file: nits_client_core::FileRef {
            repo_id: repo_id(),
            path: path("src/a.rs"),
        },
        first_row: 0,
        last_row: 59,
    }))
    .unwrap();
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Diff {
            row: 4,
            side: Side::Head,
        },
    }))
    .unwrap();
    // `V` enters Visual on the focused row; motions extend the selection
    // from the anchor.
    press(&mut core, "V").unwrap();
    assert_eq!(core.view().mode, Mode::Visual);
    assert_eq!(
        core.view().visual,
        Some(VisualView {
            start: 4,
            end: 4,
            side: Side::Head
        })
    );
    press(&mut core, "j").unwrap();
    assert_eq!(
        core.view().visual,
        Some(VisualView {
            start: 4,
            end: 5,
            side: Side::Head
        })
    );
    press(&mut core, "k k").unwrap();
    assert_eq!(
        core.view().visual,
        Some(VisualView {
            start: 3,
            end: 4,
            side: Side::Head
        })
    );
    // `c` opens a draft anchored to the selected line range and clears the
    // selection; the composer takes the keys (Insert).
    press(&mut core, "c").unwrap();
    assert_eq!(core.view().focus, Focus::Composer);
    assert_eq!(core.view().mode, Mode::Insert);
    assert_eq!(core.view().visual, None);
    let draft = core.view().draft.clone().unwrap();
    let Anchor::Lines { lines, .. } = draft.anchor else {
        panic!(
            "visual comment should anchor to lines, got {:?}",
            draft.anchor
        );
    };
    assert!(lines.start() <= lines.end());
    // Discard; `V` then `esc` leaves Visual without a draft.
    press(&mut core, "esc").unwrap();
    press(&mut core, "V").unwrap();
    assert_eq!(core.view().mode, Mode::Visual);
    press(&mut core, "esc").unwrap();
    assert_eq!(core.view().mode, Mode::Normal);
    assert_eq!(core.view().visual, None);
    assert!(core.view().draft.is_none());
}

#[test]
fn search_step_moves_the_content_search_selection() {
    let mut core = ready();
    let effects = core
        .handle(Input::User(Action::ContentSearch {
            query: Some("l1".into()),
            all_files: false,
        }))
        .unwrap();
    let id = effects
        .iter()
        .find_map(|e| match e {
            Effect::Send(ClientMsg::Request { id, .. }) => Some(*id),
            _ => None,
        })
        .unwrap();
    let hit = |line: u32| nits_protocol::ContentHit {
        repo_id: repo_id(),
        path: path("src/a.rs"),
        line: nits_protocol::LineNo::new(line).unwrap(),
        text: "l1".into(),
    };
    core.handle(Input::Server(ServerMsg::Response {
        id,
        response: nits_protocol::Response::Search {
            hits: vec![hit(1), hit(2), hit(3)],
            truncated: false,
        },
    }))
    .unwrap();
    assert_eq!(core.view().content_search.as_ref().unwrap().selected, 0);
    let effects = core
        .handle(Input::User(Action::SearchStep {
            search: nits_client_core::SearchKind::Content,
            delta: 1,
        }))
        .unwrap();
    assert_eq!(core.view().content_search.as_ref().unwrap().selected, 1);
    assert_eq!(rendered(&effects), vec![ViewSection::Search]);
}

/// The row indices of `chunk()`'s hand-shaped rows.
const MODIFIED_ROW: u32 = 4;
const ADDED_ROW: u32 = 6;
const REMOVED_ROW: u32 = 8;

/// The core with `src/a.rs` open and the diff focused on `row`/`side`.
fn on_row(row: u32, side: Side) -> ClientCore {
    let mut core = ready();
    core.handle(Input::User(Action::Viewport {
        file: nits_client_core::FileRef {
            repo_id: repo_id(),
            path: path("src/a.rs"),
        },
        first_row: 0,
        last_row: 59,
    }))
    .unwrap();
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Diff { row, side },
    }))
    .unwrap();
    core
}

fn draft_anchor(core: &ClientCore) -> Anchor {
    core.view().draft.clone().expect("a draft is open").anchor
}

#[test]
fn comment_on_a_modified_row_anchors_to_the_focused_side() {
    // The green half is the default; `c` anchors to the head blob.
    let mut core = on_row(MODIFIED_ROW, Side::Head);
    press(&mut core, "c").unwrap();
    let Anchor::Lines {
        side,
        blob_oid,
        lines,
        ..
    } = draft_anchor(&core)
    else {
        panic!("expected a line anchor");
    };
    assert_eq!(side, Side::Head);
    assert_eq!(blob_oid, blob(11));
    assert_eq!(lines.start().get(), 5);

    // Left is an alias of `h`: same row, base blob.
    let mut core = on_row(MODIFIED_ROW, Side::Head);
    press(&mut core, "left").unwrap();
    assert_eq!(
        core.view().focus,
        Focus::Diff {
            row: MODIFIED_ROW,
            side: Side::Base
        }
    );
    press(&mut core, "c").unwrap();
    let Anchor::Lines {
        side,
        blob_oid,
        lines,
        ..
    } = draft_anchor(&core)
    else {
        panic!("expected a line anchor");
    };
    assert_eq!(side, Side::Base);
    assert_eq!(blob_oid, blob(10));
    assert_eq!(lines.start().get(), 5);
}

#[test]
fn vim_and_arrow_bindings_select_the_same_side_of_a_modified_row() {
    for (key, from, to, command) in [
        ("h", Side::Head, Side::Base, Command::SideBase),
        ("left", Side::Head, Side::Base, Command::SideBase),
        ("l", Side::Base, Side::Head, Command::SideHead),
        ("right", Side::Base, Side::Head, Command::SideHead),
    ] {
        let mut core = on_row(MODIFIED_ROW, from);
        press(&mut core, key).unwrap();
        assert_eq!(
            core.view().focus,
            Focus::Diff {
                row: MODIFIED_ROW,
                side: to
            },
            "{key}"
        );
        assert_eq!(
            core.view().last_key.as_ref().and_then(|last| last.command),
            Some(command),
            "{key} must resolve to the typed side-selection command"
        );
    }
}

#[test]
fn a_row_with_one_cell_has_no_other_side_to_move_to() {
    // An added row has no base cell, a removed row no head cell.
    for key in ["h", "left"] {
        let mut core = on_row(ADDED_ROW, Side::Head);
        assert!(press(&mut core, key).is_err(), "{key}");
        assert_eq!(
            core.view().focus,
            Focus::Diff {
                row: ADDED_ROW,
                side: Side::Head
            },
            "{key}"
        );
    }
    for key in ["l", "right"] {
        let mut core = on_row(REMOVED_ROW, Side::Base);
        assert!(press(&mut core, key).is_err(), "{key}");
        assert_eq!(
            core.view().focus,
            Focus::Diff {
                row: REMOVED_ROW,
                side: Side::Base
            },
            "{key}"
        );
    }
}

/// The rows of `chunk()`: the expander sits at index 148, between the
/// hunk headers at 0 and 149.
const EXPANDER_ROW: u32 = 148;

#[test]
fn enter_on_an_expander_opens_that_hidden_run() {
    // The mouse can click an expander, so the keyboard has to reach it.
    let core = on_row(EXPANDER_ROW, Side::Head);
    let action = nits_client_core::resolve_command(&core, nits_client_core::Command::Open).unwrap();
    assert_eq!(
        action,
        Action::ExpandGap {
            file: nits_client_core::FileRef {
                repo_id: repo_id(),
                path: path("src/a.rs"),
            },
            gap: Gap::new(1),
            dir: ExpandDir::Both,
        }
    );
}

#[test]
fn an_open_draft_marks_the_rows_it_is_about() {
    // The composer renders under the anchor row, so the core has to say
    // which rows the draft covers — a component cannot work that out
    // without redoing the anchoring.
    let mut core = on_row(MODIFIED_ROW, Side::Base);
    press(&mut core, "V").unwrap();
    press(&mut core, "j").unwrap();
    press(&mut core, "c").unwrap();
    let rows = &core.view().diff.as_ref().expect("a file is open").rows;
    let drafted: Vec<(u32, nits_client_core::RowPlace, Side)> = rows
        .iter()
        .filter_map(|r| r.drafted.map(|(place, side)| (r.index, place, side)))
        .collect();
    assert_eq!(
        drafted,
        vec![
            (MODIFIED_ROW, nits_client_core::RowPlace::Inside, Side::Base),
            (
                MODIFIED_ROW + 1,
                nits_client_core::RowPlace::Anchor,
                Side::Base
            ),
        ],
        "both selected rows are marked, the last one carrying the composer"
    );
    // Discarding clears them.
    press(&mut core, "esc").unwrap();
    assert!(
        core.view()
            .diff
            .as_ref()
            .is_some_and(|d| d.rows.iter().all(|r| r.drafted.is_none()))
    );
}

#[test]
fn a_review_level_draft_marks_no_rows() {
    // It has no line to sit under, so it keeps the docked composer.
    let mut core = on_row(MODIFIED_ROW, Side::Head);
    core.handle(Input::User(Action::DraftOpened {
        anchor: Anchor::Review,
    }))
    .unwrap();
    assert!(
        core.view()
            .diff
            .as_ref()
            .is_some_and(|d| d.rows.iter().all(|r| r.drafted.is_none()))
    );
}

#[test]
fn the_scroll_family_records_an_intent_the_host_performs() {
    let mut core = on_row(4, Side::Head);
    assert_eq!(core.view().scroll, None);

    press(&mut core, "z z").unwrap();
    let centred = core.view().scroll.expect("z z records an intent");
    assert_eq!(centred.row, 4);
    assert_eq!(centred.align, nits_client_core::ScrollAlign::Center);

    // The cursor does not move — only the view does.
    assert_eq!(
        core.view().focus,
        Focus::Diff {
            row: 4,
            side: Side::Head
        }
    );

    // Repeating the chord is a new instruction: the reader may have
    // scrolled away with the mouse since.
    press(&mut core, "z z").unwrap();
    let again = core.view().scroll.expect("still an intent");
    assert_eq!((again.row, again.align), (centred.row, centred.align));
    assert_eq!(again.seq, centred.seq + 1);

    press(&mut core, "z t").unwrap();
    assert_eq!(
        core.view().scroll.map(|s| s.align),
        Some(nits_client_core::ScrollAlign::Top)
    );
    press(&mut core, "z b").unwrap();
    assert_eq!(
        core.view().scroll.map(|s| s.align),
        Some(nits_client_core::ScrollAlign::Bottom)
    );
}

#[test]
fn focus_settles_on_the_side_the_row_has() {
    // Stepping onto an added row from the red half lands on its green
    // cell — the focus never points at a cell that is not there.
    let mut core = on_row(MODIFIED_ROW, Side::Base);
    press(&mut core, "j").unwrap(); // row 5, context: both sides exist
    assert_eq!(
        core.view().focus,
        Focus::Diff {
            row: 5,
            side: Side::Base
        }
    );
    press(&mut core, "j").unwrap(); // row 6, added: head only
    assert_eq!(
        core.view().focus,
        Focus::Diff {
            row: ADDED_ROW,
            side: Side::Head
        }
    );
}

#[test]
fn a_visual_selection_keeps_the_side_it_started_on() {
    // Started on the red half of a modified row: the range is base lines,
    // and the removed row it crosses contributes its line.
    let mut core = on_row(MODIFIED_ROW, Side::Base);
    press(&mut core, "V").unwrap();
    assert_eq!(
        core.view().visual,
        Some(nits_client_core::VisualView {
            start: MODIFIED_ROW,
            end: MODIFIED_ROW,
            side: Side::Base
        })
    );
    // Down to the removed row, crossing an added row that has no base cell.
    for _ in 0..4 {
        press(&mut core, "j").unwrap();
    }
    let effects = press(&mut core, "c").unwrap();
    assert!(effects.iter().any(|e| matches!(e, Effect::Render(_))));
    let Anchor::Lines { side, lines, .. } = draft_anchor(&core) else {
        panic!("expected a line anchor");
    };
    assert_eq!(side, Side::Base);
    assert_eq!((lines.start().get(), lines.end().get()), (5, 9));
}

#[test]
fn the_side_cannot_be_flipped_mid_selection() {
    let mut core = on_row(MODIFIED_ROW, Side::Head);
    press(&mut core, "V").unwrap();
    assert!(press(&mut core, "h").is_err());
    assert_eq!(
        core.view().visual.map(|v| v.side),
        Some(Side::Head),
        "the selection keeps the side it started on"
    );
}

#[test]
fn opening_a_thread_in_another_file_lands_on_its_side() {
    // The base-side thread on b.rs, which is not the open file: the
    // transition that opens the file has to carry the side, since the
    // focus cannot be set once the rows arrive.
    let mut core = ready();
    let index = core
        .view()
        .threads
        .iter()
        .position(|t| {
            matches!(&t.place, nits_client_core::ThreadPlace::Lines { file, side, .. }
                if file.path == path("b.rs") && *side == Side::Base)
        })
        .expect("the b.rs thread is listed");
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Thread { index },
    }))
    .unwrap();
    press(&mut core, "enter").unwrap();
    assert_eq!(
        core.view().diff.as_ref().map(|d| d.file.path.clone()),
        Some(path("b.rs")),
        "the thread's file is open"
    );
    assert_eq!(
        core.view().focus,
        Focus::Diff {
            row: 4,
            side: Side::Base
        },
        "on the thread's row and side, not the top of the window"
    );
}

#[test]
fn the_scroll_family_means_nothing_off_the_diff() {
    let mut core = ready();
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Tree { index: 0 },
    }))
    .unwrap();
    // `z` in the tree is CollapseAll, so the chord does not even reach the
    // scroll family; dispatching the action directly is the real test.
    assert!(
        core.handle(Input::User(Action::ScrollView {
            align: nits_client_core::ScrollAlign::Center,
        }))
        .is_err()
    );
    assert_eq!(core.view().scroll, None);
}

#[test]
fn stepping_over_a_file_boundary_follows_but_a_jump_pins() {
    // `j` off the end of a file is a continuous motion: the next file
    // opens with no reposition, so the view scrolls by one row.
    let mut core = on_row(4, Side::Head);
    core.handle(Input::User(Action::RunCommand {
        command: nits_client_core::Command::GoBottom,
    }))
    .unwrap();
    let before = core.view().scroll;
    press(&mut core, "j").unwrap();
    assert_eq!(
        core.view().diff.as_ref().map(|d| d.file.path.clone()),
        Some(path("b.rs")),
        "the motion carried into the next file"
    );
    assert_eq!(
        core.view().scroll,
        before,
        "a motion over the boundary records no reposition"
    );

    // `] f` is a deliberate jump to a distant file: its header goes to
    // the top of the viewport.
    let mut core = on_row(4, Side::Head);
    press(&mut core, "] f").unwrap();
    let pinned = core.view().scroll.expect("a jump pins the file");
    assert_eq!(
        (pinned.row, pinned.align),
        (0, nits_client_core::ScrollAlign::Top)
    );

    // So is opening a file from the tree.
    let mut core = ready();
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Tree { index: 0 },
    }))
    .unwrap();
    press(&mut core, "j j").unwrap(); // into src/a.rs
    press(&mut core, "enter").unwrap();
    assert_eq!(
        core.view().scroll.map(|s| s.align),
        Some(nits_client_core::ScrollAlign::Top),
        "a file picked out of the tree is pinned"
    );
}

#[test]
fn the_directional_expands_open_the_gap_beside_the_cursor() {
    // Inside the second hunk: `z u` opens the run above it. Both used to
    // resolve to the same undirected whole-file re-render.
    let core = on_row(160, Side::Head);
    let up = nits_client_core::resolve_command(&core, nits_client_core::Command::ExpandUp).unwrap();
    assert_eq!(
        up,
        Action::ExpandGap {
            file: nits_client_core::FileRef {
                repo_id: repo_id(),
                path: path("src/a.rs"),
            },
            gap: Gap::new(1),
            dir: ExpandDir::Up,
        }
    );
    // Above the gap, `z d` opens the same run from the other end.
    let core = on_row(100, Side::Head);
    let down =
        nits_client_core::resolve_command(&core, nits_client_core::Command::ExpandDown).unwrap();
    assert_eq!(
        down,
        Action::ExpandGap {
            file: nits_client_core::FileRef {
                repo_id: repo_id(),
                path: path("src/a.rs"),
            },
            gap: Gap::new(1),
            dir: ExpandDir::Down,
        }
    );
    // Nothing hidden that way: the command means nothing rather than
    // re-rendering the file.
    let core = on_row(160, Side::Head);
    assert!(
        nits_client_core::resolve_command(&core, nits_client_core::Command::ExpandDown).is_err()
    );
}

#[test]
fn re_opening_the_file_already_open_keeps_its_window_and_pins_it() {
    // `enter` on the tree item for the open file (and its mouse alias,
    // the tree click, which runs the same command) must not throw the
    // reader back to row 0 — it lands on the window they are looking at,
    // pinned so a file scrolled off screen comes back.
    let mut core = on_row(120, Side::Head);
    let first_row = core.view().diff.as_ref().expect("a file is open").first_row;
    assert!(first_row > 0, "the viewport has moved off the start");
    let want = path("src/a.rs");
    let index = nits_client_core::visible_nodes(core.view())
        .iter()
        .position(|n| matches!(n, nits_client_core::TreeNode::File { path, .. } if *path == want))
        .expect("a.rs is in the tree");
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Tree { index },
    }))
    .unwrap();
    press(&mut core, "enter").unwrap();
    assert_eq!(
        core.view().focus,
        Focus::Diff {
            row: first_row,
            side: Side::Head
        }
    );
    assert_eq!(
        core.view().scroll.map(|s| s.align),
        Some(nits_client_core::ScrollAlign::Top),
        "picking a file out of the tree pins it, open or not"
    );
}

#[test]
fn opening_a_gap_re_renders_only_that_file_with_the_gap_recorded() {
    let mut core = on_row(EXPANDER_ROW, Side::Head);
    press(&mut core, "enter").unwrap();
    let opts = core
        .view()
        .review
        .as_ref()
        .and_then(|o| o.open_file.as_ref())
        .map(|f| f.render.opts.clone())
        .expect("a file is open");
    assert_eq!(
        opts.expanded.of(Gap::new(1)),
        (20, 20),
        "both ends of gap 1 opened"
    );
    assert_eq!(
        opts.context_lines,
        nits_protocol::RenderOpts::default().context_lines,
        "the file's own context is untouched — other hunks do not move"
    );
}

#[test]
fn the_gap_beside_the_cursor_is_found_without_its_chunk() {
    // `b.rs` has a header and no chunks: `diff.rows` is empty, so nothing
    // about the gap can come from the rows. A hunk taller than what the
    // cache holds is the same situation, and the chord still means the
    // gap bordering the cursor.
    let mut core = ready();
    core.handle(Input::User(Action::Viewport {
        file: nits_client_core::FileRef {
            repo_id: repo_id(),
            path: path("b.rs"),
        },
        first_row: 140,
        last_row: 199,
    }))
    .unwrap();
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Diff {
            row: 170,
            side: Side::Head,
        },
    }))
    .unwrap();
    assert!(
        core.view()
            .diff
            .as_ref()
            .is_some_and(|d| d.rows.is_empty() && !d.missing.is_empty()),
        "no chunk of this file is cached"
    );
    let up = nits_client_core::resolve_command(&core, nits_client_core::Command::ExpandUp).unwrap();
    assert_eq!(
        up,
        Action::ExpandGap {
            file: nits_client_core::FileRef {
                repo_id: repo_id(),
                path: path("b.rs"),
            },
            gap: Gap::new(1),
            dir: ExpandDir::Up,
        }
    );
    // Below the last gap there is nothing to open that way.
    assert!(
        nits_client_core::resolve_command(&core, nits_client_core::Command::ExpandDown).is_err()
    );
}

/// The file's rows once a gap has been opened: the same rows, with
/// `extra` more context lines revealed just before the second hunk.
fn expanded_rows(extra: u32) -> Vec<Row> {
    use nits_protocol::{Cell, LineNo};
    let base: Vec<Row> = (0..3).flat_map(|c| chunk(c).rows).collect();
    let revealed = |i: u32| {
        let cell = Cell {
            line_no: LineNo::new(1000 + i).unwrap(),
            text: format!("revealed {i}"),
            spans: Vec::new(),
            changed: Vec::new(),
        };
        Row::Context {
            left: cell.clone(),
            right: cell,
        }
    };
    let mut out = base[..EXPANDER_ROW as usize].to_vec();
    out.extend((0..extra).map(revealed));
    out.extend_from_slice(&base[EXPANDER_ROW as usize..]);
    out
}

/// The same expansion from the leading side of the gap: revealed rows
/// land below a cursor above it, so neither its row nor viewport shifts.
fn expanded_rows_below(extra: u32) -> Vec<Row> {
    use nits_protocol::{Cell, LineNo};
    let base: Vec<Row> = (0..3).flat_map(|c| chunk(c).rows).collect();
    let revealed = |i: u32| {
        let cell = Cell {
            line_no: LineNo::new(2000 + i).unwrap(),
            text: format!("revealed below {i}"),
            spans: Vec::new(),
            changed: Vec::new(),
        };
        Row::Context {
            left: cell.clone(),
            right: cell,
        }
    };
    let split = EXPANDER_ROW as usize + 1;
    let mut out = base[..split].to_vec();
    out.extend((0..extra).map(revealed));
    out.extend_from_slice(&base[split..]);
    out
}

/// An expanded render where the old head-side line also occurs among the
/// newly revealed rows. Only the base-side identity uniquely names the
/// split row the reader actually focused.
fn expanded_rows_with_head_decoy(extra: u32) -> Vec<Row> {
    use nits_protocol::{Cell, LineNo};
    let mut rows = expanded_rows(extra);
    let right = Cell {
        line_no: LineNo::new(171).unwrap(),
        text: "same head line, different row".to_owned(),
        spans: Vec::new(),
        changed: Vec::new(),
    };
    rows[EXPANDER_ROW as usize] = Row::Added { right };
    rows
}

/// Answer the render request in flight with a file `extra` rows longer,
/// returning what the core emitted for it.
fn deliver_render(core: &mut ClientCore, effects: &[Effect], extra: u32) -> Vec<Effect> {
    deliver_rows(core, effects, &expanded_rows(extra))
}

/// Answer the render request with the supplied complete render.
fn deliver_rows(core: &mut ClientCore, effects: &[Effect], rows: &[Row]) -> Vec<Effect> {
    let id = effects
        .iter()
        .find_map(|e| match e {
            Effect::Send(ClientMsg::Request {
                id,
                request: Request::FileRender { .. },
            }) => Some(*id),
            _ => None,
        })
        .expect("a render was requested");
    let total = u32::try_from(rows.len()).unwrap();
    let gap_row = rows
        .iter()
        .position(|row| matches!(row, Row::Expander { .. }))
        .and_then(|row| u32::try_from(row).ok())
        .expect("the shaped render keeps its expander");
    let mut header = header("src/a.rs");
    header.opts = core
        .view()
        .review
        .as_ref()
        .and_then(|o| o.open_file.as_ref())
        .map(|f| f.render.opts.clone())
        .expect("a file is open");
    header.content = RenderContent::Text {
        total_rows: total,
        chunk_rows: 100,
        chunk_count: total.div_ceil(100),
        highlighted: false,
        additions: 1,
        deletions: 1,
        gaps: nits_protocol::GapTable::try_from(vec![nits_protocol::GapRow {
            gap: Gap::new(1),
            row: gap_row,
        }])
        .unwrap(),
    };
    let mut out = core
        .handle(Input::Server(ServerMsg::StreamItem {
            id,
            item: StreamItem::Header { header },
        }))
        .unwrap();
    for (ci, rows) in rows.chunks(100).enumerate() {
        out.extend(
            core.handle(Input::Server(ServerMsg::StreamItem {
                id,
                item: StreamItem::Chunk {
                    repo_id: repo_id(),
                    path: path("src/a.rs"),
                    chunk: RenderChunk {
                        index: ChunkIndex::new(u32::try_from(ci).unwrap()),
                        rows: rows.to_vec(),
                    },
                },
            }))
            .unwrap(),
        );
    }
    if let Ok(end) = core.handle(Input::Server(ServerMsg::StreamEnd { id })) {
        out.extend(end);
    }
    out
}

#[test]
fn the_cursor_keeps_its_line_when_a_gap_above_it_opens() {
    // Opening a gap inserts rows above everything below it. The cursor
    // must stay on the line it was on — moving it is the "everything
    // shifts under you" behaviour this change is meant to remove.
    let core = &mut on_row(160, Side::Head);
    let text_at = |core: &ClientCore, row: u32| -> Option<String> {
        core.view()
            .diff
            .as_ref()?
            .rows
            .iter()
            .find(|r| r.index == row)
            .and_then(|r| match &r.row {
                Row::Context { right, .. } => Some(right.text.clone()),
                Row::HunkHeader { .. }
                | Row::Expander { .. }
                | Row::Added { .. }
                | Row::Removed { .. }
                | Row::Modified { .. }
                | Row::WhitespaceOnly => None,
            })
    };
    let before = text_at(core, 160).expect("a context row");
    let first_row = core.view().diff.as_ref().unwrap().first_row;
    let effects = press(core, "z u").unwrap();
    let landed = deliver_render(core, &effects, 20);
    // The host is told: without the focus patch it would keep the cursor
    // on the old row number over the new rows.
    let sections = rendered(&landed);
    assert!(sections.contains(&ViewSection::Focus), "{sections:?}");
    assert!(sections.contains(&ViewSection::Diff), "{sections:?}");
    assert_eq!(
        text_at(core, 180).as_deref(),
        Some(before.as_str()),
        "the line moved down by the rows the gap revealed"
    );
    assert_eq!(
        core.view().focus,
        Focus::Diff {
            row: 180,
            side: Side::Head
        },
        "and the cursor moved with it"
    );
    assert_eq!(
        core.view().diff.as_ref().unwrap().first_row,
        first_row + 20,
        "the viewport followed, so the row keeps its place on screen"
    );
    // And what the host reads for those sections is the moved cursor and
    // the shifted window, in this pass rather than after the next input.
    match core.view().patch(ViewSection::Focus) {
        nits_client_core::ViewPatch::Focus { focus, .. } => {
            assert_eq!(
                focus,
                Focus::Diff {
                    row: 180,
                    side: Side::Head
                }
            );
        }
        other => panic!("expected a focus patch, got {other:?}"),
    }
    match core.view().patch(ViewSection::Diff) {
        nits_client_core::ViewPatch::Diff { diff, .. } => {
            assert_eq!(diff.map(|d| d.first_row), Some(first_row + 20));
        }
        other => panic!("expected a diff patch, got {other:?}"),
    }

    // Repeating the operation anchors from the position produced by the
    // previous one rather than accumulating a row of drift each time.
    let effects = press(core, "z u").unwrap();
    let landed = deliver_render(core, &effects, 40);
    assert!(rendered(&landed).contains(&ViewSection::Focus));
    assert_eq!(
        core.view().focus,
        Focus::Diff {
            row: 200,
            side: Side::Head
        }
    );
    assert_eq!(core.view().diff.as_ref().unwrap().first_row, first_row + 40);
    assert_eq!(
        text_at(core, 200).as_deref(),
        Some(before.as_str()),
        "the second expansion preserves the same logical line"
    );
}

#[test]
fn repeated_expansion_before_the_first_render_lands_keeps_the_original_line() {
    let core = &mut on_row(160, Side::Head);
    let file = core.view().diff.as_ref().unwrap().file.clone();
    let before = core
        .view()
        .diff
        .as_ref()
        .unwrap()
        .rows
        .iter()
        .find(|row| row.index == 160)
        .and_then(|row| match &row.row {
            Row::Context { right, .. } => Some(right.text.clone()),
            Row::HunkHeader { .. }
            | Row::Expander { .. }
            | Row::Added { .. }
            | Row::Removed { .. }
            | Row::Modified { .. }
            | Row::WhitespaceOnly => None,
        })
        .unwrap();

    core.handle(Input::User(Action::ExpandGap {
        file: file.clone(),
        gap: Gap::new(1),
        dir: nits_protocol::ExpandDir::Up,
    }))
    .unwrap();
    assert!(core.view().diff.is_none(), "the first render is pending");
    let second = core
        .handle(Input::User(Action::ExpandGap {
            file,
            gap: Gap::new(1),
            dir: nits_protocol::ExpandDir::Up,
        }))
        .unwrap();
    let landed = deliver_render(core, &second, 40);

    assert!(rendered(&landed).contains(&ViewSection::Focus));
    assert_eq!(
        core.view().focus,
        Focus::Diff {
            row: 200,
            side: Side::Head,
        }
    );
    let focused = core
        .view()
        .diff
        .as_ref()
        .unwrap()
        .rows
        .iter()
        .find(|row| row.index == 200)
        .and_then(|row| match &row.row {
            Row::Context { right, .. } => Some(right.text.as_str()),
            Row::HunkHeader { .. }
            | Row::Expander { .. }
            | Row::Added { .. }
            | Row::Removed { .. }
            | Row::Modified { .. }
            | Row::WhitespaceOnly => None,
        });
    assert_eq!(focused, Some(before.as_str()));
}

#[test]
fn opening_a_gap_below_keeps_the_exact_focus_and_viewport() {
    let core = &mut on_row(100, Side::Head);
    let first_row = core.view().diff.as_ref().unwrap().first_row;
    let effects = press(core, "z d").unwrap();
    let landed = deliver_rows(core, &effects, &expanded_rows_below(20));
    let sections = rendered(&landed);
    assert!(sections.contains(&ViewSection::Diff), "{sections:?}");
    assert!(sections.contains(&ViewSection::Focus), "{sections:?}");
    assert_eq!(
        core.view().focus,
        Focus::Diff {
            row: 100,
            side: Side::Head
        }
    );
    assert_eq!(core.view().diff.as_ref().unwrap().first_row, first_row);
    match core.view().patch(ViewSection::Diff) {
        nits_client_core::ViewPatch::Diff { diff, .. } => {
            assert_eq!(diff.map(|d| d.first_row), Some(first_row));
        }
        other => panic!("expected a diff patch, got {other:?}"),
    }
}

#[test]
fn realignment_preserves_the_focused_side_of_a_split_row() {
    let core = &mut on_row(170, Side::Base);
    let first_row = core.view().diff.as_ref().unwrap().first_row;
    let effects = press(core, "z u").unwrap();
    deliver_rows(core, &effects, &expanded_rows_with_head_decoy(20));
    assert_eq!(
        core.view().focus,
        Focus::Diff {
            row: 190,
            side: Side::Base
        },
        "the newly revealed duplicate head line must not steal base focus"
    );
    assert_eq!(core.view().diff.as_ref().unwrap().first_row, first_row + 20);
}

/// Open `b.rs`, returning the chunk requests it made. Its rows arrive
/// separately (`deliver_b_rs`) so a test can focus a row in between.
fn request_b_rs(core: &mut ClientCore) -> Vec<(nits_protocol::RequestId, u32)> {
    let effects = core
        .handle(Input::User(Action::Viewport {
            file: nits_client_core::FileRef {
                repo_id: repo_id(),
                path: path("b.rs"),
            },
            first_row: 0,
            last_row: 199,
        }))
        .unwrap();
    // b.rs's header is already cached, so only its chunks are asked for.
    let wanted: Vec<(nits_protocol::RequestId, u32)> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::Send(ClientMsg::Request {
                id,
                request: Request::RenderChunk { index, .. },
            }) => Some((*id, index.get())),
            _ => None,
        })
        .collect();
    assert!(!wanted.is_empty(), "b.rs's chunks were asked for");
    wanted
}

/// b.rs's rows: the same line numbers as `src/a.rs`, five rows earlier,
/// so a realign meant for that file would visibly move the cursor here.
fn deliver_b_rs(core: &mut ClientCore, wanted: Vec<(nits_protocol::RequestId, u32)>) {
    use nits_protocol::{Cell, LineNo, Response};
    let row = |n: u32| {
        let cell = Cell {
            line_no: LineNo::new(n).unwrap(),
            text: format!("l{n}"),
            spans: Vec::new(),
            changed: Vec::new(),
        };
        Row::Context {
            left: cell.clone(),
            right: cell,
        }
    };
    for (id, index) in wanted {
        core.handle(Input::Server(ServerMsg::Response {
            id,
            response: Response::RenderChunk {
                chunk: RenderChunk {
                    index: ChunkIndex::new(index),
                    rows: (0..100).map(|i| row(index * 100 + i + 6)).collect(),
                },
            },
        }))
        .unwrap();
    }
}

#[test]
fn a_realign_belongs_to_the_render_that_asked_for_it() {
    // Expand a.rs, then open b.rs before the response lands, and put the
    // cursor somewhere in it. b.rs holds the same line numbers five rows
    // earlier, so a realign that is not bound to the render it was
    // recorded for would drag this cursor when b.rs's rows arrive.
    let core = &mut on_row(160, Side::Head);
    press(core, "z u").unwrap();
    let wanted = request_b_rs(core);
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Diff {
            row: 100,
            side: Side::Head,
        },
    }))
    .unwrap();
    deliver_b_rs(core, wanted);
    assert_eq!(
        core.view().diff.as_ref().map(|d| d.file.path.clone()),
        Some(path("b.rs"))
    );
    assert_eq!(
        core.view().focus,
        Focus::Diff {
            row: 100,
            side: Side::Head
        },
        "a.rs's pending realign leaves b.rs's cursor where it is"
    );
}

#[test]
fn the_view_says_what_y_would_copy_from_here() {
    // The clipboard is the shell's and a write only works inside the
    // gesture that asks for it, so the shell cannot wait for a round trip
    // to learn the path. The core still decides *which* file it is.
    let mut core = ready();
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Tree { index: 2 },
    }))
    .unwrap();
    let focused = nits_client_core::visible_nodes(core.view())
        .get(2)
        .and_then(|n| match n {
            nits_client_core::TreeNode::File { path, .. } => Some(path.clone()),
            nits_client_core::TreeNode::Dir { .. } => None,
        })
        .expect("a file is focused");
    assert_eq!(core.view().copy_target.as_ref(), Some(&focused));

    // It follows the focus rather than the last copy: moving to a review
    // with no file leaves nothing to copy.
    let effects = core
        .handle(Input::User(Action::SetFocus {
            focus: Focus::ReviewList { index: 0 },
        }))
        .unwrap();
    assert!(rendered(&effects).contains(&ViewSection::Focus));
    assert_eq!(core.view().copy_target, None);

    // `y` itself is a no-op in the core: the shell has already copied.
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Tree { index: 2 },
    }))
    .unwrap();
    // The only patch is the key count: the copy itself happened in the
    // shell, inside the gesture.
    let effects = press(&mut core, "y").unwrap();
    assert_eq!(rendered(&effects), vec![ViewSection::Hints]);
    assert_eq!(core.view().copy_target.as_ref(), Some(&focused));
}

#[test]
fn the_view_lists_the_bindings_that_apply_where_the_focus_is() {
    // A host that must decide what a key means before the core answers
    // resolves against this. `hints` is primary bindings only (`y` is not
    // one) and `chrome` is one entry per command with no context, so
    // either would have `y` copying on a focused thread, where the
    // keymap binds nothing and the core refuses the key.
    let mut core = ready();
    let bound = |core: &ClientCore, command: nits_client_core::Command| {
        core.view().bindings.iter().any(|b| b.command == command)
    };
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Tree { index: 2 },
    }))
    .unwrap();
    assert!(bound(&core, nits_client_core::Command::CopyPath));
    assert!(
        !core
            .view()
            .hints
            .iter()
            .any(|h| h.command == nits_client_core::Command::CopyPath),
        "`y` is not a primary binding, so the hint bar does not carry it"
    );

    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Thread { index: 0 },
    }))
    .unwrap();
    assert!(
        !bound(&core, nits_client_core::Command::CopyPath),
        "the thread keymap binds no copy, and the core refuses the key there"
    );
    assert!(press(&mut core, "y").is_err());
    // The chrome, by contrast, carries `y` everywhere — which is why the
    // shell must not resolve against it.
    assert!(
        core.view()
            .chrome
            .iter()
            .any(|h| h.command == nits_client_core::Command::CopyPath)
    );
}

#[test]
fn every_key_the_core_acts_on_is_counted_with_what_it_meant() {
    // A host that acts on a key before the core answers compares this
    // count against what it has sent, and reads the command rather than
    // predicting one. A command that changes nothing still has to be
    // counted, or the count stalls on it and the host waits for ever; a
    // key the context does not bind stays a typed error and is not
    // counted, and the host — resolving against the same bindings —
    // does not count that one either.
    let mut core = ready();
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Tree { index: 2 },
    }))
    .unwrap();
    let start = core.view().last_key.map_or(0, |k| k.seq);

    // `y` is a no-op in the core (the shell has already copied) and is
    // still counted, with a patch to carry the verdict.
    let effects = press(&mut core, "y").unwrap();
    assert_eq!(
        core.view().last_key,
        Some(LastKey {
            seq: start + 1,
            command: Some(Command::CopyPath),
        })
    );
    assert!(rendered(&effects).contains(&ViewSection::Hints));

    // A prefix and the chord completing it are two, and only the second
    // resolved to anything.
    press(&mut core, "g").ok();
    assert_eq!(
        core.view().last_key,
        Some(LastKey {
            seq: start + 2,
            command: None,
        })
    );
    press(&mut core, "g").ok();
    assert_eq!(
        core.view().last_key,
        Some(LastKey {
            seq: start + 3,
            command: Some(Command::GoTop),
        })
    );

    // An unbound key stays an error, and is still counted and recorded:
    // the host counts the keys it sends, and it sent this one. There is
    // no patch — the rejection returns before the view is derived — but
    // the view carries the verdict, so a host attaching now reads the
    // sequence the core is actually at.
    assert!(core.handle(Input::Key(KeyChord::char('q'))).is_err());
    assert_eq!(
        core.view().last_key,
        Some(LastKey {
            seq: start + 4,
            command: None,
        }),
        "a rejected key is counted, and the view says where the core is"
    );
}

#[test]
fn a_shell_attaching_after_a_rejected_key_reads_the_sequence_the_core_is_at() {
    // The attach re-emits the view as it stands, without deriving, so a
    // key the core rejected has to be in the view already: a host that
    // adopted the last broadcast sequence would count from behind, and
    // mistake the next key's verdict for its own.
    let mut core = ready();
    press(&mut core, "j").unwrap();
    assert!(core.handle(Input::Key(KeyChord::char('q'))).is_err());

    let attached = core
        .view()
        .full_patches()
        .into_iter()
        .find_map(|p| {
            if let nits_client_core::ViewPatch::Hints { last_key, .. } = p {
                Some(last_key)
            } else {
                None
            }
        })
        .expect("the attach carries the hints section");
    assert_eq!(attached, core.view().last_key);
    assert_eq!(
        attached.map(|k| k.seq),
        Some(2),
        "both keys counted, the rejected one included"
    );
}

#[test]
fn a_key_that_lands_where_it_is_unbound_reports_no_command() {
    // The case a host cannot predict: `y` typed straight after `g t`
    // resolves against the diff's bindings on the way out, but lands in
    // the thread list, where it means nothing. The core says so, so the
    // host copies nothing rather than the file it was looking at.
    let mut core = ready();
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Diff {
            row: 0,
            side: Side::Head,
        },
    }))
    .unwrap();
    assert_eq!(
        core.view().focus.context(),
        Context::Diff,
        "y is bound here"
    );

    press(&mut core, "g").ok();
    press(&mut core, "t").ok();
    assert_eq!(core.view().focus.context(), Context::Thread);

    let waiting = core.view().last_key.map_or(0, |k| k.seq) + 1;
    assert!(
        core.handle(Input::Key(KeyChord::char('y'))).is_err(),
        "y is not bound in the thread list"
    );

    // The verdict on that key is what stops a host copying the file it
    // happened to be looking at: the key was acted on, and it meant
    // nothing.
    assert_eq!(
        core.view().last_key,
        Some(LastKey {
            seq: waiting,
            command: None,
        })
    );
}

#[test]
fn comment_navigation_stops_once_per_thread() {
    // The thread on line 5 of a.rs covers rows 2..=4 once its range is
    // more than one line; `] c` must stop on the row its card hangs
    // under, not on every line it marks.
    let mut core = on_row(0, Side::Head);
    let rows = core
        .view()
        .diff
        .as_ref()
        .expect("a file is open")
        .rows
        .clone();
    let marked: Vec<u32> = rows
        .iter()
        .filter(|r| !r.threads.is_empty())
        .map(|r| r.index)
        .collect();
    let anchors: Vec<u32> = rows
        .iter()
        .filter(|r| {
            r.threads
                .iter()
                .any(|t| t.place == nits_client_core::RowPlace::Anchor)
        })
        .map(|r| r.index)
        .collect();
    assert!(
        marked.len() > anchors.len(),
        "the fixture has a multi-line thread: {marked:?} vs {anchors:?}"
    );
    press(&mut core, "] c").unwrap();
    let Focus::Diff { row, .. } = core.view().focus else {
        panic!("still on the diff")
    };
    assert!(
        anchors.contains(&row),
        "landed on {row}, which is not a card's row ({anchors:?})"
    );
    // And the next one skips the rest of that thread's rows.
    assert!(press(&mut core, "] c").is_err() || !marked.contains(&(row + 1)));
}

/// The one content query sent by an action, with its exact ordered effects.
fn search_request(core: &mut ClientCore, query: &str) -> nits_protocol::RequestId {
    let effects = core
        .handle(Input::User(Action::ContentSearch {
            query: Some(query.into()),
            all_files: false,
        }))
        .unwrap();
    let Effect::Send(ClientMsg::Request {
        id,
        request:
            Request::Search {
                review_id: requested_review,
                query: requested_query,
                all_files,
                scope,
            },
    }) = &effects[0]
    else {
        panic!("expected Search request, got {effects:?}");
    };
    let id = *id;
    assert_eq!(*requested_review, review_id());
    assert_eq!(requested_query, query);
    assert!(!all_files);
    assert_eq!(
        effects,
        vec![
            Effect::Send(ClientMsg::Request {
                id,
                request: Request::Search {
                    review_id: review_id(),
                    query: query.into(),
                    all_files: false,
                    scope: *scope
                }
            }),
            Effect::Render(nits_client_core::ViewDelta {
                sections: vec![ViewSection::Search]
            }),
        ]
    );
    id
}

fn search_answer(
    core: &mut ClientCore,
    id: nits_protocol::RequestId,
    lines: &[u32],
) -> Vec<Effect> {
    core.handle(Input::Server(ServerMsg::Response {
        id,
        response: nits_protocol::Response::Search {
            hits: lines
                .iter()
                .map(|line| nits_protocol::ContentHit {
                    repo_id: repo_id(),
                    path: path("src/a.rs"),
                    line: nits_protocol::LineNo::new(*line).unwrap(),
                    text: format!("line {line}"),
                })
                .collect(),
            truncated: false,
        },
    }))
    .unwrap()
}

#[test]
fn content_results_accept_only_the_latest_request_even_after_close_and_reopen() {
    let mut core = ready();
    let old = search_request(&mut core, "old");
    let current = search_request(&mut core, "current");
    assert_eq!(search_answer(&mut core, old, &[1, 2]), vec![]);
    assert!(core.view().content_search.as_ref().unwrap().pending);
    assert_eq!(
        search_answer(&mut core, current, &[3]),
        vec![Effect::Render(nits_client_core::ViewDelta {
            sections: vec![ViewSection::Search]
        })]
    );
    assert_eq!(
        core.view().content_search.as_ref().unwrap().hits[0]
            .line
            .get(),
        3
    );

    let old = search_request(&mut core, "same");
    assert_eq!(
        core.handle(Input::User(Action::ContentSearch {
            query: None,
            all_files: false
        }))
        .unwrap(),
        vec![Effect::Render(nits_client_core::ViewDelta {
            sections: vec![ViewSection::Search]
        })]
    );
    let current = search_request(&mut core, "same");
    assert_eq!(
        search_answer(&mut core, current, &[2]),
        vec![Effect::Render(nits_client_core::ViewDelta {
            sections: vec![ViewSection::Search]
        })]
    );
    assert_eq!(search_answer(&mut core, old, &[1]), vec![]);
    assert_eq!(
        core.view().content_search.as_ref().unwrap().hits[0]
            .line
            .get(),
        2
    );
}

#[test]
fn search_activation_uses_current_selection_and_rejects_stale_or_pending_queries() {
    use nits_client_core::SearchKind;
    let mut core = ready();
    let mut expected = ready();
    for client in [&mut core, &mut expected] {
        client
            .handle(Input::User(Action::FileSearch {
                query: Some("rs".into()),
            }))
            .unwrap();
        client
            .handle(Input::User(Action::SearchStep {
                search: SearchKind::Files,
                delta: 1,
            }))
            .unwrap();
    }
    assert_eq!(
        core.handle(Input::User(Action::OpenSearchResult {
            search: SearchKind::Files,
            query: "old".into()
        }))
        .unwrap(),
        vec![]
    );
    let file = expected.view().tree.search.as_ref().unwrap().hits[1]
        .file
        .clone();
    let expected_effects = expected
        .handle(Input::User(Action::Viewport {
            file: file.clone(),
            first_row: 0,
            last_row: 59,
        }))
        .unwrap();
    assert_eq!(
        core.handle(Input::User(Action::OpenSearchResult {
            search: SearchKind::Files,
            query: "rs".into()
        }))
        .unwrap(),
        expected_effects
    );
    assert_eq!(core.view(), expected.view());

    let request = search_request(&mut core, "query");
    assert_eq!(
        core.handle(Input::User(Action::OpenSearchResult {
            search: SearchKind::Content,
            query: "query".into()
        }))
        .unwrap(),
        vec![]
    );
    assert_eq!(
        search_answer(&mut core, request, &[]),
        vec![Effect::Render(nits_client_core::ViewDelta {
            sections: vec![ViewSection::Search]
        })]
    );
    assert_eq!(
        core.handle(Input::User(Action::OpenSearchResult {
            search: SearchKind::Content,
            query: "query".into()
        }))
        .unwrap(),
        vec![]
    );
}

#[test]
fn content_selection_is_independent_of_an_underlying_file_search() {
    use nits_client_core::SearchKind;
    let mut core = ready();
    core.handle(Input::User(Action::FileSearch {
        query: Some("rs".into()),
    }))
    .unwrap();
    let request = search_request(&mut core, "query");
    search_answer(&mut core, request, &[1, 2]);
    assert_eq!(
        core.handle(Input::User(Action::SearchStep {
            search: SearchKind::Content,
            delta: 1
        }))
        .unwrap(),
        vec![Effect::Render(nits_client_core::ViewDelta {
            sections: vec![ViewSection::Search]
        })]
    );
    assert_eq!(core.view().content_search.as_ref().unwrap().selected, 1);
    assert_eq!(core.view().tree.search.as_ref().unwrap().selected, 0);
}

#[test]
fn entering_search_results_resets_the_core_selection_without_a_view_round_trip() {
    use nits_client_core::{SearchKind, ViewDelta};
    for search in [SearchKind::Files, SearchKind::Content] {
        let mut core = ready();
        let section = match search {
            SearchKind::Files => {
                core.handle(Input::User(Action::FileSearch {
                    query: Some("rs".into()),
                }))
                .unwrap();
                ViewSection::Tree
            }
            SearchKind::Content => {
                let id = search_request(&mut core, "query");
                search_answer(&mut core, id, &[1, 2]);
                ViewSection::Search
            }
        };
        // The host published selection zero, then handles navigation while
        // withholding subsequent views from its UI. Re-entering results must
        // reset to zero regardless of what that published view still contains.
        let published = core.view().clone();
        core.handle(Input::User(Action::SearchFirst { search }))
            .unwrap();
        assert_eq!(
            core.handle(Input::User(Action::SearchStep { search, delta: 1 }))
                .unwrap(),
            vec![Effect::Render(ViewDelta {
                sections: vec![section]
            })]
        );
        assert_eq!(
            core.handle(Input::User(Action::SearchFirst { search }))
                .unwrap(),
            vec![Effect::Render(ViewDelta {
                sections: vec![section]
            })]
        );
        assert_eq!(core.view(), &published);
    }
}

#[test]
fn informational_and_review_finding_composers_have_keyboard_commands() {
    use nits_protocol::CommentIntent;
    let mut core = ready();
    press(&mut core, "space i").unwrap();
    let draft = core.view().draft.as_ref().unwrap();
    assert_eq!(draft.anchor, Anchor::Review);
    assert_eq!(draft.intent, CommentIntent::Informational);
    assert_eq!(core.view().focus, Focus::Composer);
    press(&mut core, "esc").unwrap();
    press(&mut core, "space f").unwrap();
    let draft = core.view().draft.as_ref().unwrap();
    assert_eq!(draft.anchor, Anchor::Review);
    assert_eq!(draft.intent, CommentIntent::Finding);
    press(&mut core, "esc").unwrap();
    press(&mut core, "space i").unwrap();
    core.handle(Input::User(Action::DraftSubmitted {
        body: "Summary".into(),
    }))
    .unwrap();
    let index = core
        .view()
        .threads
        .iter()
        .position(|t| t.status == nits_client_core::ThreadStatus::Informational)
        .unwrap();
    core.handle(Input::User(Action::SetFocus {
        focus: Focus::Thread { index },
    }))
    .unwrap();
    assert!(
        !core
            .view()
            .hints
            .iter()
            .any(|h| h.command == Command::ToggleResolved)
    );
    assert!(nits_client_core::resolve_command(&core, Command::ToggleResolved).is_err());
    press(&mut core, "r").unwrap();
    assert_eq!(
        core.view().draft.as_ref().unwrap().reply_to,
        Some(core.view().threads[index].id)
    );
}

#[test]
fn requests_have_keyboard_navigation_and_open_changes_without_resolving_findings() {
    let mut core = ready();
    let findings = core.view().threads.clone();
    for seq in 100..102 {
        core.handle(Input::Server(ServerMsg::Event {
            event: nits_protocol::Event {
                seq: Seq::new(seq),
                ts: Timestamp::from_millis(1_700_000_000_000),
                author: Author::Human {
                    name: "ada".into(),
                    machine: "box".into(),
                },
                client_id: ClientId::from_parts(9, 9),
                client_seq: nits_protocol::ClientSeq::new(seq),
                body: nits_protocol::EventBody::ReviewRequested {
                    review_id: review_id(),
                    agent: "review-agent".into(),
                    note: format!("Request {seq}"),
                },
            },
        }))
        .unwrap();
    }
    press(&mut core, "g r").unwrap();
    assert_eq!(core.view().focus, Focus::ReviewRequest { index: 0 });
    assert_eq!(core.view().tab, nits_client_core::Tab::Conversation);
    press(&mut core, "j").unwrap();
    assert_eq!(core.view().focus, Focus::ReviewRequest { index: 1 });
    press(&mut core, "k").unwrap();
    assert_eq!(core.view().focus, Focus::ReviewRequest { index: 0 });
    assert!(
        core.view()
            .hints
            .iter()
            .any(|hint| hint.command == Command::Open)
    );
    assert!(
        !core
            .view()
            .hints
            .iter()
            .any(|hint| hint.command == Command::ToggleResolved)
    );
    press(&mut core, "enter").unwrap();
    assert_eq!(core.view().tab, nits_client_core::Tab::FilesChanged);
    assert!(matches!(core.view().focus, Focus::Diff { .. }));
    assert_eq!(core.view().threads, findings);
    assert_eq!(core.view().requests.len(), 2);
}
