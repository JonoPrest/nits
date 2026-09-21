//! Cancellation owns all connection work, on real Unix and WebSocket transports.

use std::future::Future;
use std::sync::{Arc, Weak};
use std::time::Duration;

use nits_protocol::{
    Author, BuildInfo, ClientId, ClientMsg, ClientSeq, Envelope, NonEmpty, ProtocolVersion,
    RefSpec, RenderOpts, RepoId, Request, Response, ReviewId, ReviewTarget, ServerMsg, Since,
    SubscribeScope, WorkspaceId,
};
use nits_review_core::DataDir;
use nits_test_support::{RepoBuilder, TestRepo, files};
use nitsd::Daemon;
use nitsd::client::{Client, ClientError, Identity};
use nitsd::codec::CodecError;
use nitsd::connection::{self, ConnectionError};
use nitsd::server::{UnixServer, WsServer};
use nitsd::transport::{self, FrameRead, FrameWrite};
use tokio::net::{TcpListener, UnixListener};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("synchronized operation must finish")
}

fn identity() -> Identity {
    Identity {
        client_id: ClientId::from_parts(1, 1),
        client: BuildInfo {
            name: "cleanup-test".into(),
            version: "0".into(),
        },
        author: Author::Human {
            name: "ada".into(),
            machine: "test".into(),
        },
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    data: DataDir,
    daemon: Arc<Daemon>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let data = DataDir::new(dir.path());
        let daemon = Daemon::open(&data, identity().client).unwrap();
        Self { dir, data, daemon }
    }

    async fn assert_released(self, weak: Weak<Daemon>) {
        drop(self.daemon);
        // Server abort may still be dropping its JoinSet on another runtime
        // worker. Observe actual ownership, not the connection counter.
        bounded(async {
            while weak.upgrade().is_some() {
                tokio::task::yield_now().await;
            }
        })
        .await;
        // The existing writer thread and any already-started Core reads finish
        // independently. Successful reopen proves that Core/redb also released
        // the store, in the very same runtime (not just at process teardown).
        bounded(async {
            loop {
                match Daemon::open(&self.data, identity().client) {
                    Ok(reopened) => break drop(reopened),
                    Err(error) if error.to_string().contains("Database already open") => {
                        tokio::task::yield_now().await;
                    }
                    Err(error) => panic!("unexpected reopen failure: {error}"),
                }
            }
        })
        .await;
    }

    async fn seed(&self) -> TestRepo {
        let repo = RepoBuilder::new()
            .commit("base", files!["a.txt" => "before\n"])
            .branch("feature")
            .commit("change", files!["a.txt" => "after\n"])
            .build()
            .unwrap();
        let path = repo.path().to_str().unwrap().to_owned();
        let ctx = Daemon::ctx(identity().author, identity().client_id, ClientSeq::new(1));
        self.daemon
            .write(move |core| {
                let workspace = WorkspaceId::from_parts(1, 1);
                let repo = RepoId::from_parts(1, 1);
                core.create_workspace(&ctx, workspace, "workspace".into())?;
                core.attach_repo(&ctx, workspace, repo, &path, "repository".into())?;
                core.create_review(
                    &ctx,
                    ReviewId::from_parts(1, 1),
                    workspace,
                    "review".into(),
                    NonEmpty::new(vec![ReviewTarget {
                        repo_id: repo,
                        base: RefSpec::Branch {
                            name: "main".into(),
                        },
                        head: RefSpec::Branch {
                            name: "feature".into(),
                        },
                    }])
                    .unwrap(),
                )
            })
            .await
            .unwrap();
        repo
    }
}

#[derive(Clone, Copy)]
enum Transport {
    Unix,
    WebSocket,
}

#[derive(Clone, Copy)]
enum Stop {
    Token,
    Abort,
}

async fn server_shutdown(transport: Transport, stop: Stop) {
    let fixture = Fixture::new();
    let weak = Arc::downgrade(&fixture.daemon);
    let shutdown = CancellationToken::new();
    let (server, client) = match transport {
        Transport::Unix => {
            let socket = fixture.dir.path().join("daemon.sock");
            let server = UnixServer::bind(&socket).unwrap();
            let task = tokio::spawn(server.run(Arc::clone(&fixture.daemon), shutdown.clone()));
            let client = bounded(Client::connect_unix(&socket, identity()))
                .await
                .unwrap();
            (task, client)
        }
        Transport::WebSocket => {
            let server = WsServer::bind("127.0.0.1:0".parse().unwrap())
                .await
                .unwrap();
            let url = format!("ws://{}", server.addr());
            let task = tokio::spawn(server.run(Arc::clone(&fixture.daemon), shutdown.clone()));
            let client = bounded(Client::connect_ws(&url, identity())).await.unwrap();
            (task, client)
        }
    };
    // This acknowledgement proves the handshake and subscription request ran.
    assert!(matches!(
        bounded(client.request(Request::Subscribe {
            scope: SubscribeScope::All,
            since: Since::Now,
        }))
        .await
        .unwrap(),
        Response::Subscribed { .. }
    ));
    match stop {
        Stop::Token => {
            shutdown.cancel();
            bounded(server).await.unwrap();
        }
        Stop::Abort => {
            server.abort();
            assert!(bounded(server).await.unwrap_err().is_cancelled());
        }
    }
    assert!(bounded(client.next_unsolicited()).await.is_none());
    assert!(matches!(
        bounded(client.request(Request::ListWorkspaces)).await,
        Err(ClientError::Closed)
    ));
    fixture.assert_released(weak).await;
}

#[tokio::test]
async fn unix_shutdown_releases_connection_and_store() {
    server_shutdown(Transport::Unix, Stop::Token).await;
}
#[tokio::test]
async fn websocket_shutdown_releases_connection_and_store() {
    server_shutdown(Transport::WebSocket, Stop::Token).await;
}
#[tokio::test]
async fn aborting_unix_server_releases_connection_and_store() {
    server_shutdown(Transport::Unix, Stop::Abort).await;
}
#[tokio::test]
async fn aborting_websocket_server_releases_connection_and_store() {
    server_shutdown(Transport::WebSocket, Stop::Abort).await;
}

#[derive(Clone, Copy)]
enum Boundary {
    Welcome,
    Response,
    Stream,
    Error,
}

impl Boundary {
    fn matches(self, message: &ServerMsg) -> bool {
        match self {
            Self::Welcome => matches!(message, ServerMsg::Welcome { .. }),
            Self::Response => matches!(message, ServerMsg::Response { .. }),
            Self::Stream => matches!(message, ServerMsg::StreamItem { .. }),
            Self::Error => matches!(message, ServerMsg::Error { .. }),
        }
    }
}

struct Gate {
    boundary: Boundary,
    reached: oneshot::Sender<()>,
    resume: oneshot::Receiver<Result<(), std::io::Error>>,
}

struct GateWrite<W> {
    write: W,
    gate: Option<Gate>,
}

impl<W: FrameWrite> FrameWrite for GateWrite<W> {
    async fn send(&mut self, frame: &[u8]) -> Result<(), CodecError> {
        let envelope: Envelope<ServerMsg> = nitsd::codec::decode(frame)?;
        if self
            .gate
            .as_ref()
            .is_some_and(|gate| gate.boundary.matches(&envelope.msg))
        {
            let gate = self.gate.take().unwrap();
            gate.reached.send(()).unwrap();
            gate.resume.await.unwrap()?;
        }
        self.write.send(frame).await
    }
    async fn close(&mut self) -> Result<(), CodecError> {
        self.write.close().await
    }
}

fn spawn_gated<R: FrameRead + 'static, W: FrameWrite + 'static>(
    daemon: Arc<Daemon>,
    read: R,
    write: W,
    gate: Gate,
) -> JoinHandle<Result<(), ConnectionError>> {
    tokio::spawn(connection::serve_framed(
        daemon,
        read,
        GateWrite {
            write,
            gate: Some(gate),
        },
    ))
}

// The wrapper only delays a known frame. All handshake, framing and disconnect
// behavior still travels over a real Unix socket or TCP WebSocket.
async fn gated_connection(
    fixture: &Fixture,
    transport: Transport,
    gate: Gate,
) -> (
    JoinHandle<Result<(), ConnectionError>>,
    JoinHandle<Result<Client, ClientError>>,
) {
    match transport {
        Transport::Unix => {
            let socket = fixture.dir.path().join("gated.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let client =
                tokio::spawn(async move { Client::connect_unix(&socket, identity()).await });
            let (socket, _) = bounded(listener.accept()).await.unwrap();
            let (read, write) = transport::byte_stream(socket);
            (
                spawn_gated(Arc::clone(&fixture.daemon), read, write, gate),
                client,
            )
        }
        Transport::WebSocket => {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("ws://{}", listener.local_addr().unwrap());
            let client = tokio::spawn(async move { Client::connect_ws(&url, identity()).await });
            let (socket, _) = bounded(listener.accept()).await.unwrap();
            let ws = bounded(tokio_tungstenite::accept_async(socket))
                .await
                .unwrap();
            let (read, write) = transport::web_socket(ws);
            (
                spawn_gated(Arc::clone(&fixture.daemon), read, write, gate),
                client,
            )
        }
    }
}

async fn blocked_handshake(transport: Transport) {
    let fixture = Fixture::new();
    let weak = Arc::downgrade(&fixture.daemon);
    let (reached, paused) = oneshot::channel();
    let (_resume, resume) = oneshot::channel();
    let (server, client) = gated_connection(
        &fixture,
        transport,
        Gate {
            boundary: Boundary::Welcome,
            reached,
            resume,
        },
    )
    .await;
    bounded(paused).await.unwrap();
    server.abort();
    assert!(bounded(server).await.unwrap_err().is_cancelled());
    assert!(bounded(client).await.unwrap().is_err());
    fixture.assert_released(weak).await;
}

#[tokio::test]
async fn aborting_handshake_closes_both_transports() {
    for transport in [Transport::Unix, Transport::WebSocket] {
        blocked_handshake(transport).await;
    }
}

async fn blocked_write(transport: Transport, boundary: Boundary, fail_writer: bool) {
    let fixture = Fixture::new();
    let _repo = fixture.seed().await;
    let weak = Arc::downgrade(&fixture.daemon);
    let (reached, paused) = oneshot::channel();
    let (resume, resume_rx) = oneshot::channel();
    let (server, connecting) = gated_connection(
        &fixture,
        transport,
        Gate {
            boundary,
            reached,
            resume: resume_rx,
        },
    )
    .await;
    let client = Arc::new(bounded(connecting).await.unwrap().unwrap());
    let waiting = match boundary {
        Boundary::Response => {
            let client = Arc::clone(&client);
            tokio::spawn(async move {
                assert!(matches!(
                    client
                        .request(Request::Subscribe {
                            scope: SubscribeScope::All,
                            since: Since::Now,
                        })
                        .await,
                    Err(ClientError::Closed)
                ));
            })
        }
        Boundary::Stream => {
            let (_, mut stream) = client
                .stream(Request::OpenReview {
                    review_id: ReviewId::from_parts(1, 1),
                    opts: RenderOpts::default(),
                })
                .await
                .unwrap();
            tokio::spawn(async move {
                assert!(matches!(
                    stream.recv().await,
                    Some(Err(ClientError::Closed))
                ));
                assert!(stream.recv().await.is_none());
            })
        }
        Boundary::Welcome | Boundary::Error => unreachable!(),
    };
    bounded(paused).await.unwrap();
    if fail_writer {
        resume
            .send(Err(std::io::ErrorKind::BrokenPipe.into()))
            .unwrap();
        assert!(bounded(server).await.unwrap().is_err());
    } else {
        server.abort();
        assert!(bounded(server).await.unwrap_err().is_cancelled());
    }
    bounded(waiting).await.unwrap();
    assert!(bounded(client.next_unsolicited()).await.is_none());
    fixture.assert_released(weak).await;
}

#[tokio::test]
async fn cancellation_fails_pending_subscriptions_on_both_transports() {
    for transport in [Transport::Unix, Transport::WebSocket] {
        blocked_write(transport, Boundary::Response, false).await;
    }
}
#[tokio::test]
async fn cancellation_fails_pending_streams_on_both_transports() {
    for transport in [Transport::Unix, Transport::WebSocket] {
        blocked_write(transport, Boundary::Stream, false).await;
    }
}
#[tokio::test]
async fn writer_failure_closes_pending_subscriptions_on_both_transports() {
    for transport in [Transport::Unix, Transport::WebSocket] {
        blocked_write(transport, Boundary::Response, true).await;
    }
}
#[tokio::test]
async fn writer_failure_closes_pending_streams_on_both_transports() {
    for transport in [Transport::Unix, Transport::WebSocket] {
        blocked_write(transport, Boundary::Stream, true).await;
    }
}

struct ObserveEof<R> {
    read: R,
    eof: Option<oneshot::Sender<()>>,
}

impl<R: FrameRead> FrameRead for ObserveEof<R> {
    async fn recv(&mut self) -> Result<Option<Vec<u8>>, CodecError> {
        let frame = self.read.recv().await?;
        if frame.is_none() {
            self.eof.take().unwrap().send(()).unwrap();
        }
        Ok(frame)
    }
}

#[tokio::test]
async fn clean_unix_eof_drains_queued_responses_in_order() {
    use tokio::io::AsyncWriteExt;
    let fixture = Fixture::new();
    let (client, server) = tokio::net::UnixStream::pair().unwrap();
    let (mut read, mut write) = transport::byte_stream(client);
    let (server_read, server_write) = transport::byte_stream(server);
    let (eof, observed_eof) = oneshot::channel();
    let (reached, paused) = oneshot::channel();
    let (resume, resume_rx) = oneshot::channel();
    let server = spawn_gated(
        Arc::clone(&fixture.daemon),
        ObserveEof {
            read: server_read,
            eof: Some(eof),
        },
        server_write,
        Gate {
            boundary: Boundary::Error,
            reached,
            resume: resume_rx,
        },
    );
    let hello = ClientMsg::Hello {
        client_id: identity().client_id,
        protocol: ProtocolVersion::CURRENT,
        client: identity().client,
        author: identity().author,
    };
    transport::send_msg(&mut write, &Envelope::current(hello.clone()))
        .await
        .unwrap();
    assert!(matches!(
        transport::recv_msg::<_, ServerMsg>(&mut read)
            .await
            .unwrap()
            .unwrap()
            .msg,
        ServerMsg::Welcome { .. }
    ));
    // Invalid Hello messages are answered inline, before EOF is observed.
    // A protocol mismatch and repeated Hello produce distinguishable replies.
    transport::send_msg(
        &mut write,
        &Envelope {
            v: ProtocolVersion::new(99, 0, 0),
            msg: hello.clone(),
        },
    )
    .await
    .unwrap();
    bounded(paused).await.unwrap();
    transport::send_msg(&mut write, &Envelope::current(hello))
        .await
        .unwrap();
    write.shutdown().await.unwrap();
    bounded(observed_eof).await.unwrap();
    assert!(
        !server.is_finished(),
        "EOF must still drain the paused writer"
    );
    resume.send(Ok(())).unwrap();
    let first = bounded(transport::recv_msg::<_, ServerMsg>(&mut read))
        .await
        .unwrap()
        .unwrap();
    let second = bounded(transport::recv_msg::<_, ServerMsg>(&mut read))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        first.msg,
        ServerMsg::Error {
            error: nits_protocol::RpcError::VersionMismatch { .. },
            ..
        }
    ));
    assert!(matches!(
        second.msg,
        ServerMsg::Error {
            error: nits_protocol::RpcError::Invalid { .. },
            ..
        }
    ));
    assert!(
        bounded(transport::recv_msg::<_, ServerMsg>(&mut read))
            .await
            .unwrap()
            .is_none()
    );
    bounded(server).await.unwrap().unwrap();
    let weak = Arc::downgrade(&fixture.daemon);
    fixture.assert_released(weak).await;
}
