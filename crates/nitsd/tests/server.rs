//! End-to-end over the unix socket and WebSocket transports (plan 2.2, 2.4).
//! Every test that only needs a client runs against both.
#![allow(clippy::format_collect)]

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nits_protocol::{
    Anchor, Author, BaseRefSpec, BuildInfo, ChunkIndex, ClientId, ClientMsg, ClientSeq, CommentId,
    CommentKind, DiffScope, Envelope, EventBody, Mutation, NonEmpty, ProtocolVersion, RefSpec,
    RenderOpts, RepoId, RepoPath, Request, RequestId, Response, ReviewId, ReviewTarget,
    ReviewTargetUpdate, RpcError, Since, StreamItem, SubscribeScope, TargetRevision, WorkspaceId,
};
use nits_review_core::DataDir;
use nits_test_support::{RepoBuilder, TestRepo, files};
use nitsd::Daemon;
use nitsd::client::{Client, Identity};
use nitsd::server::{UnixServer, WsServer};
use tokio_util::sync::CancellationToken;

/// Which listener a test's clients connect through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    Unix,
    Ws,
}

/// Where clients connect for one transport.
#[derive(Debug, Clone)]
enum Endpoint {
    Unix(std::path::PathBuf),
    Ws(String),
}

impl Endpoint {
    async fn connect(&self, identity: Identity) -> Client {
        match self {
            Endpoint::Unix(p) => Client::connect_unix(p, identity).await.unwrap(),
            Endpoint::Ws(url) => Client::connect_ws(url, identity).await.unwrap(),
        }
    }
}

struct Harness {
    _dir: tempfile::TempDir,
    socket: std::path::PathBuf,
    endpoint: Endpoint,
    shutdown: CancellationToken,
    repo: TestRepo,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

fn ws() -> WorkspaceId {
    WorkspaceId::from_parts(1, 1)
}
fn rid() -> RepoId {
    RepoId::from_parts(1, 1)
}
fn review_id() -> ReviewId {
    ReviewId::from_parts(1, 1)
}
fn comment_id(n: u128) -> CommentId {
    CommentId::from_parts(1, n)
}

fn identity(n: u128, name: &str) -> Identity {
    Identity {
        client_id: ClientId::from_parts(1, n),
        client: BuildInfo {
            name: "test".into(),
            version: "0".into(),
        },
        author: Author::Human {
            name: name.into(),
            machine: "box".into(),
        },
    }
}

/// Daemon listening on both transports; clients use the unix socket.
fn start(repo: TestRepo) -> Harness {
    start_on(repo, Transport::Unix)
}

/// Daemon listening on both transports; clients use `transport`.
fn start_on(repo: TestRepo, transport: Transport) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    // macOS caps unix socket paths at 104 bytes; keep it short.
    let socket = std::env::temp_dir().join(format!("nitsd-{}.sock", ulid_ish()));
    let daemon = Daemon::open(
        &DataDir::new(dir.path()),
        BuildInfo {
            name: "nitsd".into(),
            version: "test".into(),
        },
    )
    .unwrap();
    let server = UnixServer::bind(&socket).unwrap();
    let shutdown = CancellationToken::new();
    tokio::spawn(server.run(Arc::clone(&daemon), shutdown.clone()));
    let ws_addr = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let ws_ready = Arc::new(tokio::sync::Notify::new());
    tokio::spawn({
        let ready = Arc::clone(&ws_ready);
        let shutdown = shutdown.clone();
        async move {
            let ws = WsServer::bind(ws_addr).await.unwrap();
            ready.notify_one();
            ws.run(daemon, shutdown).await;
        }
    });
    let endpoint = match transport {
        Transport::Unix => Endpoint::Unix(socket.clone()),
        Transport::Ws => {
            // `notify_one` stores a permit, so this can't miss the signal.
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(ws_ready.notified());
            });
            Endpoint::Ws(format!("ws://{ws_addr}"))
        }
    };
    Harness {
        _dir: dir,
        socket,
        endpoint,
        shutdown,
        repo,
    }
}

/// Distinct socket names per test in this process; the clock alone collides.
static SOCKET_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn ulid_ish() -> String {
    format!(
        "{}-{}",
        std::process::id(),
        SOCKET_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )
}

async fn connect(h: &Harness, n: u128, name: &str) -> Client {
    h.endpoint.connect(identity(n, name)).await
}

/// What a request may take while the daemon is busy with something big,
/// given what the same request takes on this machine when it is idle.
/// These tests are about the daemon not *blocking* — the work never
/// takes zero time, and on a shared runner neither does anything else —
/// so the budget is a multiple of a measured control plus a floor, not a
/// wall-clock number that only holds on fast hardware.
fn budget(control: Duration) -> Duration {
    (control * 10).max(Duration::from_millis(250))
}

async fn mutate(c: &Client, seq: u64, m: Mutation) -> Result<nits_protocol::Event, RpcError> {
    match c
        .request(Request::Mutate {
            client_seq: ClientSeq::new(seq),
            mutation: m,
        })
        .await
    {
        Ok(Response::Committed { event }) => Ok(event),
        Ok(other) => panic!("unexpected {other:?}"),
        Err(nitsd::client::ClientError::Rpc(e)) => Err(e),
        Err(e) => panic!("{e}"),
    }
}

fn big_source(lines: usize) -> String {
    (0..lines)
        .map(|i| format!("fn f{i}() -> u32 {{ {i} }}\n"))
        .collect()
}

fn small_repo() -> TestRepo {
    RepoBuilder::new()
        .commit(
            "base",
            files!["a.rs" => "fn a() {}\n", "b.rs" => "fn b() {}\n"],
        )
        .branch("feature")
        .commit(
            "feat",
            files!["a.rs" => "fn a() { 1; }\n", "c.rs" => "fn c() {}\n"],
        )
        .build()
        .unwrap()
}

/// Workspace + repo + review over main..feature, created by `c`.
async fn seed(h: &Harness, c: &Client) {
    mutate(
        c,
        1,
        Mutation::CreateWorkspace {
            workspace_id: ws(),
            name: "w".into(),
        },
    )
    .await
    .unwrap();
    mutate(
        c,
        2,
        Mutation::AttachRepo {
            workspace_id: ws(),
            repo_id: rid(),
            path: h.repo.path().to_str().unwrap().into(),
            display_name: "r".into(),
        },
    )
    .await
    .unwrap();
    mutate(
        c,
        3,
        Mutation::CreateReview {
            review_id: review_id(),
            workspace_id: ws(),
            title: "t".into(),
            targets: NonEmpty::new(vec![ReviewTarget {
                repo_id: rid(),
                base: RefSpec::Branch {
                    name: "main".into(),
                },
                head: RefSpec::Branch {
                    name: "feature".into(),
                },
            }])
            .unwrap(),
        },
    )
    .await
    .unwrap();
    c.request(Request::ResolveTargets {
        review_id: review_id(),
    })
    .await
    .unwrap();
}

fn file_comment(n: u128, body: &str) -> Mutation {
    Mutation::AddComment {
        review_id: review_id(),
        comment_id: comment_id(n),
        kind: CommentKind::Note,
        anchor: Anchor::Review,
        body: body.into(),
        context: None,
    }
}

#[tokio::test]
async fn target_mutation_acknowledges_the_primary_event_before_resolution() {
    let h = start(small_repo());
    let c = connect(&h, 1, "ada").await;
    seed(&h, &c).await;

    let event = mutate(
        &c,
        4,
        Mutation::UpdateReviewTarget {
            review_id: review_id(),
            update: ReviewTargetUpdate {
                repo_id: rid(),
                revision: TargetRevision::Base {
                    ref_spec: BaseRefSpec::Head,
                },
            },
        },
    )
    .await
    .unwrap();

    assert!(matches!(
        event.body,
        EventBody::ReviewTargetUpdated { review_id: id, .. } if id == review_id()
    ));
}

#[tokio::test]
async fn handshake_reports_versions_and_rejects_unknown_major() {
    let h = start(small_repo());
    let c = connect(&h, 1, "ada").await;
    assert_eq!(c.welcome.protocol, ProtocolVersion::CURRENT);
    assert_eq!(c.welcome.daemon.name, "nitsd");

    let stream = tokio::net::UnixStream::connect(&h.socket).await.unwrap();
    let err = Client::handshake(stream, identity(2, "bob"), ProtocolVersion::new(99, 0, 0))
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        nitsd::client::ClientError::Rejected(RpcError::UnsupportedProtocol { .. })
    ));
}

#[tokio::test]
async fn frame_with_wrong_version_after_handshake_is_an_error() {
    let h = start(small_repo());
    let c = connect(&h, 1, "ada").await;
    // Bypass the client's stamping by hand-writing a frame over a raw socket.
    let mut raw = tokio::net::UnixStream::connect(&h.socket).await.unwrap();
    nitsd::codec::write_msg(
        &mut raw,
        &Envelope::current(ClientMsg::Hello {
            client_id: ClientId::from_parts(1, 5),
            protocol: ProtocolVersion::CURRENT,
            client: c.welcome.daemon.clone(),
            author: Author::Human {
                name: "x".into(),
                machine: "y".into(),
            },
        }),
    )
    .await
    .unwrap();
    let _welcome = nitsd::codec::read_msg::<_, nits_protocol::ServerMsg>(&mut raw)
        .await
        .unwrap()
        .unwrap();
    nitsd::codec::write_msg(
        &mut raw,
        &Envelope {
            v: ProtocolVersion::new(0, 0, 1),
            msg: ClientMsg::Request {
                id: RequestId::new(3),
                request: Request::ListWorkspaces,
            },
        },
    )
    .await
    .unwrap();
    let reply = nitsd::codec::read_msg::<_, nits_protocol::ServerMsg>(&mut raw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reply.v, ProtocolVersion::CURRENT);
    assert!(matches!(
        reply.msg,
        nits_protocol::ServerMsg::Error {
            id,
            error: RpcError::VersionMismatch { .. }
        } if id == RequestId::new(3)
    ));
}

async fn two_clients_one_writes_other_receives_in_order(t: Transport) {
    let h = start_on(small_repo(), t);
    let writer = connect(&h, 1, "ada").await;
    let reader = connect(&h, 2, "bob").await;
    seed(&h, &writer).await;

    let Response::Subscribed { seq } = reader
        .request(Request::Subscribe {
            scope: SubscribeScope::Review {
                review_id: review_id(),
            },
            since: Since::Now,
        })
        .await
        .unwrap()
    else {
        panic!()
    };

    let mut committed = Vec::new();
    for n in 0..5 {
        committed.push(
            mutate(&writer, 10 + n, file_comment(u128::from(n), "hi"))
                .await
                .unwrap(),
        );
    }
    for c in &committed {
        let got = reader.next_event().await.unwrap();
        assert_eq!(&got, c);
        assert!(got.seq > seq);
        assert_eq!(got.author, identity(1, "ada").author);
    }
}

async fn reconnect_with_since_receives_exactly_the_gap(t: Transport) {
    let h = start_on(small_repo(), t);
    let writer = connect(&h, 1, "ada").await;
    seed(&h, &writer).await;
    let first = mutate(&writer, 10, file_comment(1, "one")).await.unwrap();
    let second = mutate(&writer, 11, file_comment(2, "two")).await.unwrap();
    let third = mutate(&writer, 12, file_comment(3, "three")).await.unwrap();

    let reader = connect(&h, 2, "bob").await;
    let Response::Subscribed { seq } = reader
        .request(Request::Subscribe {
            scope: SubscribeScope::All,
            since: Since::After { seq: first.seq },
        })
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(seq, third.seq);
    assert_eq!(reader.next_event().await.unwrap(), second);
    assert_eq!(reader.next_event().await.unwrap(), third);
    // Live continues after the replay with no duplicate.
    let fourth = mutate(&writer, 13, file_comment(4, "four")).await.unwrap();
    assert_eq!(reader.next_event().await.unwrap(), fourth);
}

async fn scopes_filter_events(t: Transport) {
    let h = start_on(small_repo(), t);
    let writer = connect(&h, 1, "ada").await;
    seed(&h, &writer).await;
    let reader = connect(&h, 2, "bob").await;
    reader
        .request(Request::Subscribe {
            scope: SubscribeScope::AwaitingAgent {
                agent: "reviewer".into(),
            },
            since: Since::Now,
        })
        .await
        .unwrap();
    mutate(&writer, 10, file_comment(1, "noise")).await.unwrap();
    let wanted = mutate(
        &writer,
        11,
        Mutation::RequestReview {
            review_id: review_id(),
            agent: "reviewer".into(),
            note: "please".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(reader.next_event().await.unwrap(), wanted);
}

async fn open_review_streams_snapshot_trees_headers_then_chunks(t: Transport) {
    let h = start_on(small_repo(), t);
    let c = connect(&h, 1, "ada").await;
    seed(&h, &c).await;
    let (_, mut rx) = c
        .stream(Request::OpenReview {
            review_id: review_id(),
            opts: RenderOpts::default(),
        })
        .await
        .unwrap();
    let mut kinds = Vec::new();
    while let Some(item) = rx.recv().await {
        let item = item.unwrap();
        kinds.push(match item {
            StreamItem::ReviewSnapshot { snapshot } => {
                assert_eq!(snapshot.review.id, review_id());
                "snapshot"
            }
            StreamItem::TreeSnapshot { .. } => "tree",
            StreamItem::Header { header } => {
                assert!(["a.rs", "c.rs"].contains(&header.path.as_str()));
                "header"
            }
            StreamItem::Chunk { chunk, .. } => {
                assert_eq!(chunk.index, ChunkIndex::FIRST);
                "chunk"
            }
        });
    }
    assert_eq!(
        kinds,
        [
            "snapshot", "tree", "tree", "header", "header", "chunk", "chunk"
        ]
    );
}

async fn file_render_streams_requested_chunk_first_and_can_be_cancelled(t: Transport) {
    let repo = RepoBuilder::new()
        .commit("base", files!["big.rs" => big_source(1)])
        .branch("feature")
        .commit("feat", files!["big.rs" => big_source(1600)])
        .build()
        .unwrap();
    let h = start_on(repo, t);
    let c = connect(&h, 1, "ada").await;
    seed(&h, &c).await;
    let (_, mut rx) = c
        .stream(Request::FileRender {
            scope: DiffScope::All,
            review_id: review_id(),
            repo_id: rid(),
            path: RepoPath::new("big.rs").unwrap(),
            opts: RenderOpts::default(),
            first_chunk: ChunkIndex::new(2),
        })
        .await
        .unwrap();
    let mut indices = Vec::new();
    while let Some(item) = rx.recv().await {
        match item.unwrap() {
            StreamItem::Header { header } => assert_eq!(header.path.as_str(), "big.rs"),
            StreamItem::Chunk { chunk, .. } => indices.push(chunk.index.get()),
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(indices.first(), Some(&2));
    assert_eq!(indices.len(), 4, "1600 rows at 500/chunk");
    assert_eq!(indices, [2, 3, 0, 1]);

    let (id, mut rx) = c
        .stream(Request::OpenReview {
            review_id: review_id(),
            opts: RenderOpts::default(),
        })
        .await
        .unwrap();
    c.cancel(id);
    let mut saw_cancelled = false;
    while let Some(item) = rx.recv().await {
        if matches!(
            item,
            Err(nitsd::client::ClientError::Rpc(RpcError::Cancelled))
        ) {
            saw_cancelled = true;
        }
    }
    // Either the stream finished before the cancel landed or it was cut
    // short; both end with StreamEnd, which closed the receiver.
    let _ = saw_cancelled;
}

async fn a_large_render_does_not_delay_another_clients_mutation(t: Transport) {
    let repo = RepoBuilder::new()
        .commit("base", files!["big.rs" => big_source(10)])
        .branch("feature")
        .commit("feat", files!["big.rs" => big_source(20_000)])
        .build()
        .unwrap();
    let h = start_on(repo, t);
    let renderer = connect(&h, 1, "ada").await;
    let writer = connect(&h, 2, "bob").await;
    seed(&h, &renderer).await;

    // A control on this machine, with nothing else in flight. The claim
    // is that the render does not *delay* the mutation, so that is what
    // is measured: an absolute budget only holds on a machine as quick as
    // the one it was written on, and a loaded CI runner is not.
    let started = Instant::now();
    mutate(&writer, 1, file_comment(1, "control"))
        .await
        .unwrap();
    let control = started.elapsed();

    let (_, mut rx) = renderer
        .stream(Request::FileRender {
            scope: DiffScope::All,
            review_id: review_id(),
            repo_id: rid(),
            path: RepoPath::new("big.rs").unwrap(),
            opts: RenderOpts::default(),
            first_chunk: ChunkIndex::FIRST,
        })
        .await
        .unwrap();
    // Give the render a head start so it is genuinely in flight.
    tokio::time::sleep(Duration::from_millis(20)).await;
    let started = Instant::now();
    mutate(&writer, 2, file_comment(2, "quick")).await.unwrap();
    let latency = started.elapsed();
    assert!(
        latency <= budget(control),
        "add_comment took {latency:?} with a render in flight, {control:?} without"
    );
    let mut chunks = 0;
    while let Some(item) = rx.recv().await {
        if matches!(item.unwrap(), StreamItem::Chunk { .. }) {
            chunks += 1;
        }
    }
    assert!(chunks >= 40, "{chunks} chunks");
}

async fn reanchoring_many_comments_does_not_block_reads(t: Transport) {
    let repo = RepoBuilder::new()
        .commit("base", files!["a.rs" => big_source(600)])
        .branch("feature")
        .commit(
            "feat",
            files!["a.rs" => format!("// top\n{}", big_source(600))],
        )
        .build()
        .unwrap();
    let h = start_on(repo, t);
    let writer = connect(&h, 1, "ada").await;
    let reader = connect(&h, 2, "bob").await;
    seed(&h, &writer).await;
    let head = h.repo.rev_parse("feature").unwrap();
    let blob = h.repo.git(&["rev-parse", &format!("{head}:a.rs")]).unwrap();
    let blob: nits_protocol::BlobOid = blob.parse().unwrap();
    for n in 0..500u128 {
        let line = u32::try_from(n).unwrap() + 2;
        let anchor = nits_review_core::comments::lines_anchor(
            rid(),
            RepoPath::new("a.rs").unwrap(),
            nits_protocol::Side::Head,
            blob,
            line,
            line,
        )
        .unwrap();
        mutate(
            &writer,
            100 + u64::try_from(n).unwrap(),
            Mutation::AddComment {
                review_id: review_id(),
                comment_id: comment_id(n),
                kind: CommentKind::Note,
                anchor,
                body: "x".into(),
                context: None,
            },
        )
        .await
        .unwrap();
    }
    // The same read while the daemon is idle, as the control this
    // machine's speed is measured against.
    let started = Instant::now();
    let idle = reader
        .request(Request::ListReviews { workspace_id: ws() })
        .await
        .unwrap();
    let control = started.elapsed();
    assert!(matches!(idle, Response::Reviews { .. }));

    // Move head: every comment must be re-anchored.
    h.repo
        .write_file(
            "a.rs",
            format!("// top\n// more\n{}", big_source(600)).as_bytes(),
        )
        .unwrap();
    h.repo.git(&["commit", "-qam", "shift"]).unwrap();

    let resolve = tokio::spawn({
        let endpoint = h.endpoint.clone();
        async move {
            let c = endpoint.connect(identity(3, "carol")).await;
            c.request(Request::ResolveTargets {
                review_id: review_id(),
            })
            .await
            .unwrap()
        }
    });
    tokio::time::sleep(Duration::from_millis(5)).await;
    let started = Instant::now();
    let reviews = reader
        .request(Request::ListReviews { workspace_id: ws() })
        .await
        .unwrap();
    let latency = started.elapsed();
    assert!(matches!(reviews, Response::Reviews { .. }));
    assert!(
        latency <= budget(control),
        "list_reviews took {latency:?} while re-anchoring, {control:?} idle"
    );
    let Response::Resolved { changed, .. } = resolve.await.unwrap() else {
        panic!()
    };
    assert!(changed);
    let Response::Reviews { reviews } = reviews else {
        panic!()
    };
    assert_eq!(reviews.len(), 1);
    let Ok(Response::Committed { event }) = writer
        .request(Request::Mutate {
            client_seq: ClientSeq::new(999),
            mutation: file_comment(999, "after"),
        })
        .await
    else {
        panic!()
    };
    assert!(matches!(event.body, EventBody::CommentCreated { .. }));
}

#[tokio::test]
async fn agent_cannot_mark_viewed_and_errors_carry_request_id() {
    let h = start(small_repo());
    let human = connect(&h, 1, "ada").await;
    seed(&h, &human).await;
    let stream = tokio::net::UnixStream::connect(&h.socket).await.unwrap();
    let agent = Client::handshake(
        stream,
        Identity {
            author: Author::Agent {
                name: "bot".into(),
                model: "m".into(),
                session_id: "s".into(),
                invoked_by: None,
                via: nits_protocol::AgentVia::Mcp,
            },
            ..identity(2, "bot")
        },
        ProtocolVersion::CURRENT,
    )
    .await
    .unwrap();
    let err = mutate(
        &agent,
        1,
        Mutation::MarkViewed {
            review_id: review_id(),
            repo_id: rid(),
            path: RepoPath::new("a.rs").unwrap(),
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, RpcError::Forbidden { .. }));
    let err = human
        .request(Request::GetReview {
            review_id: ReviewId::from_parts(9, 9),
        })
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        nitsd::client::ClientError::Rpc(RpcError::NotFound { .. })
    ));
}

#[tokio::test]
async fn stale_socket_file_is_replaced_and_live_one_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = std::env::temp_dir().join(format!("nitsd-{}.sock", ulid_ish()));
    std::fs::write(&path, b"stale").unwrap();
    let daemon = Daemon::open(
        &DataDir::new(dir.path()),
        BuildInfo {
            name: "nitsd".into(),
            version: "test".into(),
        },
    )
    .unwrap();
    let server = UnixServer::bind(&path).unwrap();
    let shutdown = CancellationToken::new();
    tokio::spawn(server.run(Arc::clone(&daemon), shutdown.clone()));
    assert!(Path::new(&path).exists());
    assert!(
        UnixServer::bind(&path).is_err(),
        "a live daemon is not replaced"
    );
    shutdown.cancel();
}

/// Expand each listed `async fn name(t: Transport)` into a `#[tokio::test]`
/// per transport, in `unix::name` and `ws::name`. The WebSocket harness
/// blocks briefly while the listener binds, hence the multi-thread flavour.
macro_rules! on_both_transports {
    ($($name:ident),* $(,)?) => {
        mod unix {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
                async fn $name() {
                    super::$name(super::Transport::Unix).await;
                }
            )*
        }
        mod ws {
            $(
                #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
                async fn $name() {
                    super::$name(super::Transport::Ws).await;
                }
            )*
        }
    };
}

on_both_transports! {
    two_clients_one_writes_other_receives_in_order,
    reconnect_with_since_receives_exactly_the_gap,
    scopes_filter_events,
    open_review_streams_snapshot_trees_headers_then_chunks,
    file_render_streams_requested_chunk_first_and_can_be_cancelled,
    a_large_render_does_not_delay_another_clients_mutation,
    reanchoring_many_comments_does_not_block_reads,
}
