//! The browser bridge against a real daemon. Every WebSocket owns a host:
//! two tabs may hold different reviews, key verdicts never cross between
//! them, preferences come from one prepared KV, and closing one tab cleans
//! up only its session.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt as _, StreamExt as _};
use nits_client_core::{
    Action, ConnectionView, FileRef, IdSeed, KeyChord, Landing, Layout, ViewModel, ViewPatch,
};
use nits_client_host::KvConfig;
use nits_protocol::{
    Author, BuildInfo, ClientId, ClientSeq, Mutation, NonEmpty, RefSpec, RepoId, RepoPath, Request,
    Response, ReviewId, ReviewTarget, Side, WorkspaceId,
};
use nits_review_core::DataDir;
use nits_test_support::{RepoBuilder, TestRepo, files};
use nitsd::Daemon;
use nitsd::client::{Client, Identity};
use nitsd::contexts::{DaemonEndpoint, StartPolicy};
use nitsd::server::UnixServer;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tokio_util::sync::CancellationToken;

fn client_info() -> BuildInfo {
    BuildInfo {
        name: "bridge-test".into(),
        version: "0".into(),
    }
}

fn author() -> Author {
    Author::Human {
        name: "ada".into(),
        machine: "box".into(),
    }
}

fn workspace_id() -> WorkspaceId {
    WorkspaceId::from_parts(1, 1)
}

fn repo_id() -> RepoId {
    RepoId::from_parts(1, 2)
}

fn review_a() -> ReviewId {
    ReviewId::from_parts(1, 3)
}

fn review_b() -> ReviewId {
    ReviewId::from_parts(1, 4)
}

struct Harness {
    dir: tempfile::TempDir,
    _repo: TestRepo,
    endpoint: DaemonEndpoint,
    shutdown: CancellationToken,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

async fn mutate(client: &Client, seq: u64, mutation: Mutation) {
    let response = client
        .request(Request::Mutate {
            client_seq: ClientSeq::new(seq),
            mutation,
        })
        .await
        .unwrap();
    assert!(matches!(response, Response::Committed { .. }));
}

async fn harness() -> Harness {
    let repo = RepoBuilder::new()
        .commit("base", files!["README.md" => "base\n"])
        .branch("feature-a")
        .commit("a", files!["a.rs" => "fn a() {}\n"])
        .checkout("main")
        .branch("feature-b")
        .commit("b", files!["b.rs" => "fn b() {}\n"])
        .build()
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("nitsd.sock");
    let daemon = Daemon::open(
        &DataDir::new(dir.path().join("data")),
        BuildInfo {
            name: "nitsd".into(),
            version: "test".into(),
        },
    )
    .unwrap();
    let shutdown = CancellationToken::new();
    let server = UnixServer::bind(&socket).unwrap();
    tokio::spawn(server.run(Arc::clone(&daemon), shutdown.clone()));
    let endpoint = DaemonEndpoint::resolve(
        &nits_config::Context::Local {
            data_dir: None,
            socket: Some(socket.clone()),
        },
        StartPolicy::RequireRunning,
    )
    .unwrap();
    let setup = Client::connect_unix(
        &socket,
        Identity {
            client_id: ClientId::from_parts(9, 9),
            client: client_info(),
            author: author(),
        },
    )
    .await
    .unwrap();
    mutate(
        &setup,
        1,
        Mutation::CreateWorkspace {
            workspace_id: workspace_id(),
            name: "reviews".into(),
        },
    )
    .await;
    mutate(
        &setup,
        2,
        Mutation::AttachRepo {
            workspace_id: workspace_id(),
            repo_id: repo_id(),
            path: repo.path().to_string_lossy().into_owned(),
            display_name: "repo".into(),
        },
    )
    .await;
    for (seq, review_id, title, branch) in [
        (3, review_a(), "review a", "feature-a"),
        (4, review_b(), "review b", "feature-b"),
    ] {
        mutate(
            &setup,
            seq,
            Mutation::CreateReview {
                review_id,
                workspace_id: workspace_id(),
                title: title.into(),
                targets: NonEmpty::singleton(ReviewTarget {
                    repo_id: repo_id(),
                    base: RefSpec::Branch {
                        name: "main".into(),
                    },
                    head: RefSpec::Branch {
                        name: branch.into(),
                    },
                }),
            },
        )
        .await;
        setup
            .request(Request::ResolveTargets { review_id })
            .await
            .unwrap();
    }
    Harness {
        dir,
        _repo: repo,
        endpoint,
        shutdown,
    }
}

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Browser {
    tx: SplitSink<Socket, Message>,
    rx: SplitStream<Socket>,
    model: ViewModel,
}

impl Browser {
    async fn connect(addr: SocketAddr) -> Self {
        let (socket, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
            .await
            .unwrap();
        let (tx, rx) = socket.split();
        let mut browser = Self {
            tx,
            rx,
            model: ViewModel::default(),
        };
        browser.send_raw(r#"{"cmd":"attach"}"#).await;
        browser
            .until(|model, _| model.connection == ConnectionView::Subscribed)
            .await;
        browser
    }

    async fn send_raw(&mut self, text: &str) {
        self.tx
            .send(Message::Text(text.to_owned().into()))
            .await
            .unwrap();
    }

    async fn dispatch(&mut self, action: &Action) {
        let action = serde_json::to_value(action).unwrap();
        let command = serde_json::json!({"cmd": "dispatch", "action": action});
        self.send_raw(&command.to_string()).await;
    }

    async fn key(&mut self, chord: KeyChord) {
        let chord = serde_json::to_value(chord).unwrap();
        let command = serde_json::json!({"cmd": "key", "chord": chord});
        self.send_raw(&command.to_string()).await;
    }

    async fn batch(&mut self) -> Vec<ViewPatch> {
        let message = tokio::time::timeout(Duration::from_secs(10), self.rx.next())
            .await
            .expect("timed out waiting for patches")
            .expect("bridge closed the socket")
            .unwrap();
        let Message::Text(text) = message else {
            panic!("expected text frame, got {message:?}")
        };
        serde_json::from_str(&text).unwrap()
    }

    async fn until(&mut self, mut done: impl FnMut(&ViewModel, &[ViewPatch]) -> bool) {
        loop {
            let patches = self.batch().await;
            for patch in patches.iter().cloned() {
                self.model.apply(patch);
            }
            if done(&self.model, &patches) {
                return;
            }
        }
    }

    async fn quiet(&mut self) {
        while let Ok(Some(Ok(Message::Text(text)))) =
            tokio::time::timeout(Duration::from_millis(30), self.rx.next()).await
        {
            let patches: Vec<ViewPatch> = serde_json::from_str(&text).unwrap();
            for patch in patches {
                self.model.apply(patch);
            }
        }
    }
}

async fn wait_for_sessions(server: &nits_client_web::Server, expected: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while server.active_sessions() != expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("session count did not settle");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tabs_hold_different_reviews_and_key_verdicts_stay_on_their_socket() {
    let h = harness().await;
    let config = nits_client_web::web_config(
        h.endpoint.clone(),
        client_info(),
        author(),
        IdSeed(42),
        KvConfig::Memory,
    );
    let bridge = nits_client_web::serve(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), config)
        .await
        .unwrap();
    let mut a = Browser::connect(bridge.addr()).await;
    let mut b = Browser::connect(bridge.addr()).await;
    wait_for_sessions(&bridge, 2).await;

    a.dispatch(&Action::OpenReview {
        review_id: review_a(),
    })
    .await;
    a.until(|model, _| model.open_review == Some(review_a()))
        .await;
    b.dispatch(&Action::OpenReview {
        review_id: review_b(),
    })
    .await;
    b.until(|model, _| model.open_review == Some(review_b()))
        .await;

    let file_a = FileRef {
        repo_id: repo_id(),
        path: RepoPath::new("a.rs").unwrap(),
    };
    let file_b = FileRef {
        repo_id: repo_id(),
        path: RepoPath::new("b.rs").unwrap(),
    };
    for (browser, file) in [(&mut a, &file_a), (&mut b, &file_b)] {
        browser
            .dispatch(&Action::OpenFileAt {
                file: file.clone(),
                row: 0,
                side: Side::Head,
                landing: Landing::Pin,
            })
            .await;
        browser
            .until(|model, _| model.copy_target.as_ref() == Some(&file.path))
            .await;
    }
    a.quiet().await;
    b.quiet().await;

    a.key(KeyChord::char('y')).await;
    a.until(|model, _| {
        model.last_key.is_some_and(|key| {
            key.seq == 1 && key.command == Some(nits_client_core::Command::CopyPath)
        })
    })
    .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(150), b.rx.next())
            .await
            .is_err(),
        "tab B received tab A's key patch"
    );
    assert_eq!(b.model.last_key, None);
    assert_eq!(a.model.copy_target.as_ref(), Some(&file_a.path));

    b.key(KeyChord::char('y')).await;
    b.until(|model, _| {
        model.last_key.is_some_and(|key| {
            key.seq == 1 && key.command == Some(nits_client_core::Command::CopyPath)
        })
    })
    .await;
    assert_eq!(b.model.copy_target.as_ref(), Some(&file_b.path));
    assert_eq!(a.model.open_review, Some(review_a()));
    assert_eq!(b.model.open_review, Some(review_b()));

    a.tx.close().await.unwrap();
    wait_for_sessions(&bridge, 1).await;
    b.dispatch(&Action::SetLayout {
        layout: Layout::Split,
    })
    .await;
    b.until(|model, _| model.prefs.layout == Layout::Split)
        .await;
    bridge.stop();
    wait_for_sessions(&bridge, 0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prepared_redb_shares_preferences_without_sharing_session_state() {
    let h = harness().await;
    let kv = h.dir.path().join("web-kv.redb");
    let config = nits_client_web::web_config(
        h.endpoint.clone(),
        client_info(),
        author(),
        IdSeed(81),
        KvConfig::Redb(kv),
    );
    let bridge = nits_client_web::serve(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), config)
        .await
        .unwrap();
    let mut first = Browser::connect(bridge.addr()).await;
    first
        .dispatch(&Action::SetLayout {
            layout: Layout::Split,
        })
        .await;
    first
        .until(|model, _| model.prefs.layout == Layout::Split)
        .await;

    // This second host is concurrent with the first. It reads the same
    // prepared redb handle; trying to open the file again would fail here.
    let second = Browser::connect(bridge.addr()).await;
    assert_eq!(second.model.prefs.layout, Layout::Split);
    assert_eq!(first.model.open_review, None);
    assert_eq!(second.model.open_review, None);
    assert_eq!(first.model.last_key, None);
    assert_eq!(second.model.last_key, None);
    bridge.stop();
    wait_for_sessions(&bridge, 0).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_commands_do_not_close_the_socket_and_assets_share_the_port() {
    let h = harness().await;
    let config = nits_client_web::web_config(
        h.endpoint.clone(),
        client_info(),
        author(),
        IdSeed(99),
        KvConfig::Memory,
    );
    let bridge = nits_client_web::serve(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), config)
        .await
        .unwrap();
    let mut browser = Browser::connect(bridge.addr()).await;
    browser.send_raw(r#"{"cmd":"nonsense"}"#).await;
    browser.send_raw(r#"{"cmd":"attach"}"#).await;
    let patches = browser.batch().await;
    assert!(!patches.is_empty());

    let mut http = tokio::net::TcpStream::connect(bridge.addr()).await.unwrap();
    tokio::io::AsyncWriteExt::write_all(&mut http, b"GET /?review=abc HTTP/1.1\r\nhost: x\r\n\r\n")
        .await
        .unwrap();
    let mut body = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut http, &mut body)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&body);
    assert!(text.starts_with("HTTP/1.1 200"));
    assert!(text.contains("<div id=\"root\">"));
    bridge.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::too_many_lines)] // One real boundary: reject, retain the draft, correct and commit.
async fn invalid_deferral_input_reports_the_error_without_committing_and_can_be_corrected() {
    let h = harness().await;
    let bridge = nits_client_web::serve(
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        nits_client_web::web_config(
            h.endpoint.clone(),
            client_info(),
            author(),
            IdSeed(111),
            KvConfig::Memory,
        ),
    )
    .await
    .unwrap();
    let mut browser = Browser::connect(bridge.addr()).await;
    browser
        .dispatch(&Action::OpenReview {
            review_id: review_a(),
        })
        .await;
    browser
        .until(|model, _| model.open_review == Some(review_a()))
        .await;
    browser
        .dispatch(&Action::DraftOpened {
            anchor: nits_protocol::Anchor::Review,
        })
        .await;
    browser
        .dispatch(&Action::DraftSubmitted {
            body: "Controller bug remains unfixed".into(),
        })
        .await;
    browser
        .until(|model, _| model.threads.first().is_some_and(|t| !t.pending))
        .await;
    let thread_id = browser.model.threads[0].id;
    let observer = Client::connect_unix(
        &h.dir.path().join("nitsd.sock"),
        Identity {
            client_id: ClientId::from_parts(9, 10),
            client: client_info(),
            author: author(),
        },
    )
    .await
    .unwrap();
    let request = Request::ReviewSnapshot {
        review_id: review_a(),
    };
    let before = observer.request(request.clone()).await.unwrap();
    browser.dispatch(&Action::DeferOpened { thread_id }).await;
    browser.until(|model, _| model.draft.is_some()).await;
    let original = browser.model.draft.clone().unwrap();
    for (reason, url, message) in [
        (
            "External follow-up",
            "example.com/issues/288",
            "Enter a complete http:// or https:// URL",
        ),
        (
            "External follow-up",
            "https://ada:secret@example.com/288",
            "without credentials or whitespace",
        ),
        (
            "bad\0reason",
            "https://example.com/288",
            "unsupported control characters",
        ),
    ] {
        // Exactly the raw JSON that either UI adapter accepts, rather than an
        // already validated Action fabricated by a successful mock callback.
        browser
            .send_raw(
                &serde_json::json!({"cmd": "dispatch", "action": {
                    "type": "DeferThread", "thread_id": thread_id,
                    "reason": reason, "tracking_url": url,
                }})
                .to_string(),
            )
            .await;
        browser
            .until(|model, _| {
                model
                    .draft
                    .as_ref()
                    .and_then(|d| d.submission_error.as_ref())
                    .is_some_and(|error| error.contains(message))
            })
            .await;
        let mut expected = original.clone();
        expected
            .submission_error
            .clone_from(&browser.model.draft.as_ref().unwrap().submission_error);
        assert_eq!(browser.model.draft, Some(expected));
        assert_eq!(browser.model.focus, nits_client_core::Focus::Composer);
        assert_eq!(
            observer.request(request.clone()).await.unwrap(),
            before,
            "invalid input must not emit even a partial event"
        );
    }
    browser
        .send_raw(
            &serde_json::json!({"cmd": "dispatch", "action": {
                "type": "DeferThread", "thread_id": thread_id,
                "reason": "External follow-up", "tracking_url": "https://example.com/issues/288",
            }})
            .to_string(),
        )
        .await;
    browser
        .until(|model, _| {
            model.draft.is_none()
                && model.threads.iter().any(|t| {
                    t.id == thread_id
                        && !t.pending
                        && matches!(t.status, nits_protocol::ThreadResolution::Deferred { .. })
                })
        })
        .await;
    let Response::ReviewSnapshot { snapshot: after } = observer.request(request).await.unwrap()
    else {
        panic!("snapshot")
    };
    let Response::ReviewSnapshot { snapshot: before } = before else {
        panic!("snapshot")
    };
    assert_eq!(after.seq.get(), before.seq.get() + 1);
    assert_eq!(after.comments, before.comments);
    assert!(
        matches!(&after.threads[0].resolution, nits_protocol::ThreadResolution::Deferred { reason, tracking_url: Some(url), .. }
        if reason.to_string() == "External follow-up" && url.to_string() == "https://example.com/issues/288")
    );
    bridge.stop();
    wait_for_sessions(&bridge, 0).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn copied_verification_reply_opens_on_another_browser_session_after_resolution() {
    let h = harness().await;
    let mut config = nits_client_web::web_config(
        h.endpoint.clone(),
        client_info(),
        author(),
        IdSeed(84),
        KvConfig::Memory,
    );
    let context = nits_protocol::ReferenceContext::named("review-box").unwrap();
    config.reference_context = Some(context.clone());
    let bridge = nits_client_web::serve(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), config)
        .await
        .unwrap();
    let mut a = Browser::connect(bridge.addr()).await;
    a.dispatch(&Action::OpenReview {
        review_id: review_a(),
    })
    .await;
    a.until(|model, _| model.open_review == Some(review_a()))
        .await;
    a.dispatch(&Action::DraftOpened {
        anchor: nits_protocol::Anchor::Review,
    })
    .await;
    a.dispatch(&Action::DraftSubmitted {
        body: "Finding to verify".into(),
    })
    .await;
    a.until(|model, _| model.threads.first().is_some_and(|t| !t.pending))
        .await;
    let thread_id = a.model.threads[0].id;
    a.dispatch(&Action::Reply {
        thread_id,
        body: "Verified the fix".into(),
    })
    .await;
    a.until(|model, _| {
        model
            .threads
            .first()
            .is_some_and(|t| t.comments.len() == 2 && !t.pending)
    })
    .await;
    let comment = &a.model.threads[0].comments[1];
    let reference = comment.reference.clone().unwrap();
    let comment_id = comment.id;
    assert_eq!(reference.context, context);
    assert_eq!(
        reference.target,
        nits_protocol::ReferenceTarget::Comment { comment_id }
    );
    a.dispatch(&Action::ResolveThread { thread_id }).await;
    a.until(|model, _| {
        model.threads.first().is_some_and(|t| {
            matches!(t.status, nits_client_core::ThreadStatus::Resolved { .. }) && !t.pending
        })
    })
    .await;
    let mut b = Browser::connect(bridge.addr()).await;
    b.dispatch(&Action::OpenReference {
        reference: reference.to_string(),
    })
    .await;
    b.until(|model, _| model.focused_comment == Some(comment_id))
        .await;
    assert_eq!(b.model.open_review, Some(review_a()));
    assert_eq!(b.model.tab, nits_client_core::Tab::Conversation);
    assert_eq!(b.model.threads[0].status, a.model.threads[0].status);
    assert_eq!(b.model.copy_reference, Some(reference));
    assert_eq!(b.model.last_error, None);
    bridge.stop();
}
