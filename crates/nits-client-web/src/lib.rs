//! HTTP + WebSocket bridge for the browser UI (dev/test; ARCHITECTURE
//! §6.2).
//!
//! One port: `GET /…` serves the embedded `ui/dist` build, `GET /ws`
//! upgrades to the WebSocket that speaks the same contract as the Tauri
//! wrapper: commands in, `view` patch batches out. Every browser WebSocket
//! owns one `nits-client-host`, [`ClientCore`](nits_client_core::ClientCore),
//! and daemon connection. Messages are JSON text frames:
//!
//! - in: `{"cmd":"dispatch","action":…}` | `{"cmd":"key","chord":…}` |
//!   `{"cmd":"attach"}`
//! - out: a bounded `ViewFrame`; fragment groups reconstruct one atomic patch batch
//!
//! `Attach` re-emits every section of that socket's core. The prepared KV
//! store is the only shared state: preferences and content-addressed disk
//! entries survive into later sessions, while focus, review, cursor and key
//! sequence state stay in one core.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use futures_util::{SinkExt as _, StreamExt as _};
use include_dir::{Dir, include_dir};
use nits_client_core::protocol::{Author, BuildInfo, ClientId};
use nits_client_core::{Action, CacheConfig, IdSeed, KeyChord, ViewDelivery};
use nits_client_host::{
    Handle, HostError, HostFactory, HostSession, HostSettings, Identity, KvConfig,
};
use nitsd::contexts::DaemonEndpoint;
use tokio::io::AsyncWriteExt as _;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::broadcast;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{create_response, write_response};
use tokio_tungstenite::tungstenite::http::{Method, header};
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_util::sync::CancellationToken;

/// The built browser UI, embedded at compile time (build `ui/dist` first).
static UI: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/dist");

mod access;
mod http;

pub use access::{BrowserOrigin, InvalidBrowserOrigin};
use http::{RequestHead, read_head};

fn mime(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, ext)| ext) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript",
        Some("css") => "text/css",
        Some("json" | "map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// Serve one asset from the embedded build; unknown paths fall back to
/// `index.html` (query strings like `?review=` land on the app shell).
async fn respond_asset(stream: &mut TcpStream, head: &RequestHead) -> std::io::Result<()> {
    let path = head.uri().path();
    let rel = path.trim_start_matches('/');
    let (body, mime): (&[u8], &str) = match UI.get_file(rel) {
        Some(f) if head.method() == Method::GET && !rel.is_empty() => (f.contents(), mime(rel)),
        _ if head.method() == Method::GET => (
            UI.get_file("index.html")
                .map_or(b"missing ui/dist" as &[u8], |f| f.contents()),
            "text/html; charset=utf-8",
        ),
        _ => {
            stream
                .write_all(b"HTTP/1.1 405 Method Not Allowed\r\ncontent-length: 0\r\n\r\n")
                .await?;
            return Ok(());
        }
    };
    let header = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: {mime}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body).await
}

/// One command from the browser; the tag is parsed once, here.
#[derive(Debug, serde::Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case", deny_unknown_fields)]
enum Command {
    Dispatch { action: serde_json::Value },
    Key { chord: KeyChord },
    Attach,
}

/// Reusable settings for browser sessions. `id_seed` seeds a monotonic
/// allocator; it is not installed directly into any core.
#[derive(Debug, Clone)]
pub struct WebConfig {
    /// Additional trusted UI origins (for example Vite's exact origin).
    /// Proxies must preserve Origin and may preserve Host or use the bridge's.
    /// Empty by default; only this listener's same-origin UI is trusted.
    pub allowed_origins: Vec<BrowserOrigin>,
    pub reference_context: Option<nits_client_core::protocol::ReferenceContext>,
    pub endpoint: DaemonEndpoint,
    pub kv: KvConfig,
    pub client: BuildInfo,
    pub author: Author,
    pub cache: CacheConfig,
    pub id_seed: IdSeed,
    pub tick: Duration,
    pub keys_file: Option<std::path::PathBuf>,
}

/// Sensible defaults for a browser bridge: the caller picks the endpoint,
/// principal and KV lifetime; every connection receives fresh session ids.
#[must_use]
pub fn web_config(
    endpoint: DaemonEndpoint,
    client: BuildInfo,
    author: Author,
    id_seed: IdSeed,
    kv: KvConfig,
) -> WebConfig {
    WebConfig {
        allowed_origins: Vec::new(),
        reference_context: None,
        endpoint,
        kv,
        client,
        author,
        cache: CacheConfig::default(),
        id_seed,
        tick: Duration::from_millis(100),
        keys_file: nits_client_host::keys_file::default_keys_path(),
    }
}

#[derive(Debug, thiserror::Error)]
enum SessionError {
    #[error("browser session id space exhausted")]
    Exhausted,
}

/// Settings shared by sessions plus the only allocator allowed to mint one.
/// Refusing at `u64::MAX` makes reuse impossible rather than wrapping.
#[derive(Debug)]
struct Sessions {
    config: WebConfig,
    host: HostFactory,
    next: AtomicU64,
}

impl Sessions {
    fn new(config: WebConfig) -> Result<Self, HostError> {
        let host = HostFactory::new(HostSettings {
            endpoint: config.endpoint.clone(),
            kv: config.kv.clone(),
            cache: config.cache.clone(),
            tick: config.tick,
            keys_file: config.keys_file.clone(),
        })?;
        Ok(Self {
            config,
            host,
            next: AtomicU64::new(0),
        })
    }

    fn next(&self) -> Result<HostSession, SessionError> {
        let session = self
            .next
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| SessionError::Exhausted)?;
        let unique = self.config.id_seed.0.wrapping_add(u128::from(session));
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        Ok(HostSession {
            identity: Identity {
                client_id: ClientId::from_parts(now, unique),
                client: self.config.client.clone(),
                author: self.config.author.clone(),
            },
            id_seed: IdSeed(unique),
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("bind {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        source: std::io::Error,
    },
    #[error("host: {0}")]
    Host(#[from] HostError),
}

/// A running bridge: the bound address and a way to stop it.
#[derive(Debug)]
pub struct Server {
    addr: SocketAddr,
    shutdown: CancellationToken,
    active_sessions: Arc<AtomicUsize>,
}

impl Server {
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn stop(&self) {
        self.shutdown.cancel();
    }

    /// Browser WebSocket sessions whose private host is still alive.
    #[must_use]
    pub fn active_sessions(&self) -> usize {
        self.active_sessions.load(Ordering::Relaxed)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

/// Accept browsers on `addr` (port 0 picks a free one). A host is started
/// only after a successful `/ws` upgrade.
pub async fn serve(addr: SocketAddr, config: WebConfig) -> Result<Server, ServeError> {
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|source| ServeError::Bind { addr, source })?;
    let addr = listener
        .local_addr()
        .map_err(|source| ServeError::Bind { addr, source })?;
    let shutdown = CancellationToken::new();
    let sessions = Arc::new(Sessions::new(config)?);
    let active_sessions = Arc::new(AtomicUsize::new(0));
    let accept_shutdown = shutdown.clone();
    let accept_sessions = Arc::clone(&sessions);
    let accept_active = Arc::clone(&active_sessions);
    tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::<()>::new();
        loop {
            tokio::select! {
                () = accept_shutdown.cancelled() => break,
                accepted = listener.accept() => match accepted {
                    Ok((stream, peer)) => {
                        let sessions = Arc::clone(&accept_sessions);
                        let active = Arc::clone(&accept_active);
                        let stop = accept_shutdown.clone();
                        tasks.spawn(async move {
                            if let Err(e) = route(stream, sessions, active, stop).await {
                                tracing::debug!(%peer, error = %e, "connection failed");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "accept failed");
                        break;
                    }
                },
            }
        }
        tasks.abort_all();
    });
    Ok(Server {
        addr,
        shutdown,
        active_sessions,
    })
}

#[derive(Debug, thiserror::Error)]
enum RouteError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error("host task exited during setup")]
    HostGone,
}

struct ActiveSession {
    active: Arc<AtomicUsize>,
    shutdown: CancellationToken,
}

impl ActiveSession {
    fn new(active: Arc<AtomicUsize>, shutdown: CancellationToken) -> Self {
        active.fetch_add(1, Ordering::Relaxed);
        Self { active, shutdown }
    }
}

impl Drop for ActiveSession {
    fn drop(&mut self) {
        self.shutdown.cancel();
        self.active.fetch_sub(1, Ordering::Relaxed);
    }
}

/// One TCP connection: `/ws` + upgrade becomes a bridge client, anything
/// else is a single asset response.
async fn route(
    mut stream: TcpStream,
    sessions: Arc<Sessions>,
    active: Arc<AtomicUsize>,
    shutdown: CancellationToken,
) -> Result<(), RouteError> {
    let (head, pending) = match read_head(&mut stream).await {
        Ok(head) => head,
        Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
            http::reject(&mut stream, "400 Bad Request").await?;
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    let access = access::Access::new(stream.local_addr()?, &sessions.config.allowed_origins);
    if !access.allows_host(head.headers()) {
        http::reject(&mut stream, "403 Forbidden").await?;
        return Ok(());
    }
    if head.uri().path() == "/ws" || head.headers().contains_key(header::UPGRADE) {
        if head.uri() != "/ws" {
            http::reject(&mut stream, "404 Not Found").await?;
            return Ok(());
        }
        if !access.allows_origin(head.headers()) {
            http::reject(&mut stream, "403 Forbidden").await?;
            return Ok(());
        }
        let Ok(response) = create_response(&head) else {
            http::reject(&mut stream, "400 Bad Request").await?;
            return Ok(());
        };
        let mut reply = Vec::new();
        write_response(&mut reply, &response).map_err(std::io::Error::other)?;
        stream.write_all(&reply).await?;
        let ws = tokio_tungstenite::WebSocketStream::from_partially_read(
            stream,
            pending,
            Role::Server,
            None,
        )
        .await;
        let session_shutdown = shutdown.child_token();
        let _active = ActiveSession::new(active, session_shutdown.clone());
        let session = sessions.next()?;
        let (handle, mut host_patches) = sessions.host.spawn(session, session_shutdown.clone());
        if let Some(context) = &sessions.config.reference_context {
            handle.dispatch(Action::SetReferenceContext {
                context: context.clone(),
            });
        }
        // A bounded per-session queue keeps a slow browser from growing the
        // host's output without limit. It has exactly one receiver and never
        // carries another session's patches.
        let (patches_tx, patches) = broadcast::channel::<Arc<ViewDelivery>>(256);
        let fan_out = patches_tx.clone();
        let fan_shutdown = session_shutdown.clone();
        let fan = tokio::spawn(async move {
            while let Some(batch) = host_patches.recv().await {
                let _ = fan_out.send(Arc::new(batch));
            }
            fan_shutdown.cancel();
        });
        if !handle.dispatch(Action::Connect) {
            return Err(RouteError::HostGone);
        }
        client(ws, handle, patches, session_shutdown.clone()).await;
        session_shutdown.cancel();
        fan.abort();
        return Ok(());
    }
    respond_asset(&mut stream, &head).await?;
    Ok(())
}

async fn client<S>(
    ws: tokio_tungstenite::WebSocketStream<S>,
    handle: Handle,
    mut patches: broadcast::Receiver<Arc<ViewDelivery>>,
    shutdown: CancellationToken,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let (mut tx, mut rx) = ws.split();
    'session: loop {
        tokio::select! {
            () = shutdown.cancelled() => break,
            batch = patches.recv() => match batch {
                Ok(batch) => {
                    // Queue whole logical groups; emit one bounded message at a
                    // time, including groups larger than the queue's capacity.
                    for frame in batch.frames() {
                        let text = match serde_json::to_string(frame) {
                            Ok(text) => text,
                            Err(error) => {
                                tracing::error!(%error, "serialize view frame");
                                break 'session;
                            }
                        };
                        tokio::select! {
                            () = shutdown.cancelled() => break 'session,
                            sent = tx.send(Message::Text(text.into())) => {
                                if sent.is_err() {
                                    break 'session;
                                }
                            }
                        }
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    // Dropped patches would corrupt the client's model copy;
                    // an attach re-emits every section.
                    tracing::warn!(dropped = n, "slow websocket client; re-attaching");
                    if !handle.attach() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            msg = rx.next() => match msg {
                Some(Ok(Message::Text(text))) => {
                    let alive = match serde_json::from_str::<Command>(&text) {
                        Ok(Command::Dispatch { action }) => handle.dispatch_json(action),
                        Ok(Command::Key { chord }) => handle.key(chord),
                        Ok(Command::Attach) => handle.attach(),
                        Err(e) => {
                            tracing::warn!(error = %e, "bad command from browser");
                            true
                        }
                    };
                    if !alive {
                        break;
                    }
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_))) => {}
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_session_gets_a_distinct_typed_identity_and_seed() {
        let sessions = Sessions::new(web_config(
            DaemonEndpoint::WebSocket {
                url: "ws://127.0.0.1:1".into(),
            },
            BuildInfo {
                name: "web-test".into(),
                version: "0".into(),
            },
            Author::Human {
                name: "ada".into(),
                machine: "test".into(),
            },
            IdSeed(700),
            KvConfig::Memory,
        ))
        .unwrap();

        let first = sessions.next().unwrap();
        let second = sessions.next().unwrap();

        let first_id: ClientId = first.identity.client_id;
        let second_id: ClientId = second.identity.client_id;
        let first_seed: IdSeed = first.id_seed;
        let second_seed: IdSeed = second.id_seed;
        assert_ne!(first_id, second_id);
        assert_ne!(first_id.random(), second_id.random());
        assert_eq!(first_seed, IdSeed(700));
        assert_eq!(second_seed, IdSeed(701));
    }

    #[test]
    fn session_allocator_refuses_to_wrap() {
        let sessions = Sessions::new(web_config(
            DaemonEndpoint::WebSocket {
                url: "ws://127.0.0.1:1".into(),
            },
            BuildInfo {
                name: "web-test".into(),
                version: "0".into(),
            },
            Author::Human {
                name: "ada".into(),
                machine: "test".into(),
            },
            IdSeed(0),
            KvConfig::Memory,
        ))
        .unwrap();
        sessions.next.store(u64::MAX, Ordering::Relaxed);

        assert!(matches!(sessions.next(), Err(SessionError::Exhausted)));
    }

    // The delivery crosses the actual writer and WebSocket framing, with a
    // deliberately tiny socket buffer. A group larger than the queue capacity
    // must finish without triggering a self-induced reattach loop.
    #[tokio::test]
    async fn large_delivery_groups_cross_the_socket_and_cancel_under_backpressure() {
        use nits_client_core::{ConnectionView, ViewBatchKind, ViewEncoder, ViewPatch};
        use nits_protocol::RpcError;

        let sessions = Sessions::new(web_config(
            DaemonEndpoint::WebSocket {
                url: "ws://127.0.0.1:1".into(),
            },
            BuildInfo {
                name: "web-test".into(),
                version: "0".into(),
            },
            Author::Human {
                name: "ada".into(),
                machine: "test".into(),
            },
            IdSeed(0),
            KvConfig::Memory,
        ))
        .unwrap();
        let shutdown = CancellationToken::new();
        let (handle, _host_output) = sessions
            .host
            .spawn(sessions.next().unwrap(), shutdown.clone());
        let (server, browser) = tokio::io::duplex(4096);
        let server =
            tokio_tungstenite::WebSocketStream::from_raw_socket(server, Role::Server, None).await;
        let mut browser =
            tokio_tungstenite::WebSocketStream::from_raw_socket(browser, Role::Client, None).await;
        let (tx, rx) = broadcast::channel(256);
        let writer = tokio::spawn(client(server, handle, rx, shutdown.clone()));
        let original = vec![ViewPatch::Connection {
            connection: ConnectionView::Subscribed,
            last_error: Some(RpcError::Internal {
                message: "source".repeat(3_000_000),
            }),
        }];
        let batch = Arc::new(
            ViewEncoder::default()
                .encode(ViewBatchKind::Snapshot, original)
                .unwrap(),
        );
        assert!(batch.frames().len() > 256);
        tx.send(Arc::clone(&batch)).unwrap();
        tokio::time::timeout(Duration::from_secs(20), async {
            for expected in batch.frames() {
                let received = browser.next().await.unwrap().unwrap().into_text().unwrap();
                assert!(received.len() < nits_client_core::VIEW_MESSAGE_LIMIT);
                let decoded: nits_client_core::ViewFrame = serde_json::from_str(&received).unwrap();
                assert_eq!(&decoded, expected);
            }
        })
        .await
        .expect("fragment group did not finish");

        // Start another group, consume its first frame to prove the writer is
        // active, then stop reading with subsequent frames still buffered.
        tx.send(batch).unwrap();
        browser.next().await.unwrap().unwrap();
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(2), writer)
            .await
            .expect("backpressured writer ignored shutdown")
            .unwrap();
    }
}
