//! Host loop around [`ClientCore`] (plan 4.3, ARCHITECTURE §5): owns the
//! transport (local socket, SSH stdio, or WebSocket), the host KV store
//! (redb file or memory), the clock, and turns every `Effect::Render` into
//! [`ViewPatch`]es for a UI. Host-agnostic: the Tauri app and the TUI wrap
//! it; the integration test drives it against a real daemon.
//!
//! One task owns the core. Everything else (socket reader/writer, the
//! ticker, the UI) talks to it over channels, so the core never blocks and
//! is never shared.

pub mod keys_file;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nits_client_core::{
    Action, Bytes, CacheConfig, ClientCore, Config, DiskTier, Effect, IdSeed, Input, KeyChord,
    Keymap, TransportEvent, ViewBatchKind, ViewDelivery, ViewEncoder, ViewPatch,
};
use nits_protocol::{Author, BuildInfo, ClientId, ClientMsg, Envelope, ServerMsg};
use nitsd::contexts::DaemonEndpoint;
use nitsd::transport::{self, FrameRead, FrameWrite};
use redb::ReadableDatabase;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Who this client is on the wire.
#[derive(Debug, Clone)]
pub struct Identity {
    pub client_id: ClientId,
    pub client: BuildInfo,
    pub author: Author,
}

/// Where the host KV lives.
#[derive(Debug, Clone)]
pub enum KvConfig {
    /// Nothing survives the process; tests and the browser fallback.
    Memory,
    /// A redb file under the host's data dir.
    Redb(PathBuf),
}

#[derive(Debug, Clone)]
pub struct HostConfig {
    pub endpoint: DaemonEndpoint,
    pub kv: KvConfig,
    pub identity: Identity,
    pub cache: CacheConfig,
    /// Entropy for the ids the core mints. Hosts pass random bits.
    pub id_seed: IdSeed,
    /// How often the core sees the clock.
    pub tick: Duration,
    /// `keys.toml` with keymap overrides (UI-DESIGN §bindings); read when
    /// the core loads its keymap. `None` disables the file.
    pub keys_file: Option<PathBuf>,
}

/// Resources shared by one or more independent host sessions.
#[derive(Debug, Clone)]
pub struct HostSettings {
    pub endpoint: DaemonEndpoint,
    pub kv: KvConfig,
    pub cache: CacheConfig,
    pub tick: Duration,
    pub keys_file: Option<PathBuf>,
}

/// Identity and id entropy private to one [`ClientCore`].
#[derive(Debug, Clone)]
pub struct HostSession {
    pub identity: Identity,
    pub id_seed: IdSeed,
}

#[derive(Debug, thiserror::Error)]
pub enum HostError {
    #[error("kv store: {0}")]
    Kv(#[from] KvError),
}

#[derive(Debug, thiserror::Error)]
pub enum KvError {
    #[error("the shared KV lock was poisoned")]
    Poisoned,
    #[error(transparent)]
    Database(#[from] redb::DatabaseError),
    #[error(transparent)]
    Transaction(#[from] redb::TransactionError),
    #[error(transparent)]
    Table(#[from] redb::TableError),
    #[error(transparent)]
    Storage(#[from] redb::StorageError),
    #[error(transparent)]
    Commit(#[from] redb::CommitError),
}

const KV_TABLE: redb::TableDefinition<'_, &str, &[u8]> = redb::TableDefinition::new("kv");

/// The host KV behind `Effect::Persist` / `Load` / `Remove`.
enum Kv {
    Memory(HashMap<String, Vec<u8>>),
    Redb(redb::Database),
}

/// One opened host KV store. Clones share the same in-memory map or redb
/// handle, so several independent UI sessions can share preferences and
/// content-addressed disk entries without opening a redb file twice.
#[derive(Clone)]
struct KvStore {
    inner: Arc<Mutex<Kv>>,
}

impl std::fmt::Debug for KvStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KvStore").finish_non_exhaustive()
    }
}

impl KvStore {
    fn open(config: &KvConfig) -> Result<Self, KvError> {
        Ok(Self {
            inner: Arc::new(Mutex::new(Kv::open(config)?)),
        })
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, KvError> {
        self.inner.lock().map_err(|_| KvError::Poisoned)?.get(key)
    }

    fn put(&self, key: &str, value: &[u8]) -> Result<(), KvError> {
        self.inner
            .lock()
            .map_err(|_| KvError::Poisoned)?
            .put(key, value)
    }

    fn remove(&self, key: &str) -> Result<(), KvError> {
        self.inner
            .lock()
            .map_err(|_| KvError::Poisoned)?
            .remove(key)
    }
}

/// Prepared shared host resources. Opening this once and spawning several
/// sessions shares only the KV backing; every spawn still owns its own core,
/// transport and client identity.
#[derive(Debug, Clone)]
pub struct HostFactory {
    endpoint: DaemonEndpoint,
    kv: KvStore,
    cache: CacheConfig,
    tick: Duration,
    keys_file: Option<PathBuf>,
}

impl HostFactory {
    pub fn new(settings: HostSettings) -> Result<Self, HostError> {
        Ok(Self {
            kv: KvStore::open(&settings.kv)?,
            endpoint: settings.endpoint,
            cache: settings.cache,
            tick: settings.tick,
            keys_file: settings.keys_file,
        })
    }

    /// Start one independent core and daemon connection on the current
    /// runtime, backed by this factory's shared KV store.
    #[must_use]
    pub fn spawn(
        &self,
        session: HostSession,
        shutdown: CancellationToken,
    ) -> (Handle, mpsc::UnboundedReceiver<ViewDelivery>) {
        let (actions_tx, actions_rx) = mpsc::unbounded_channel();
        let (patches_tx, patches_rx) = mpsc::unbounded_channel();
        let mut core = ClientCore::new(Config {
            client_id: session.identity.client_id,
            client: session.identity.client,
            author: session.identity.author,
            id_seed: session.id_seed,
            cache: self.cache.clone(),
        });
        if let Ok(context) = self.endpoint.reference_context() {
            // Host identity is installed before the first attach/connect.
            let _ = core.handle(Input::User(Action::SetReferenceContext { context }));
        }
        let host = Host {
            core,
            endpoint: self.endpoint.clone(),
            kv: self.kv.clone(),
            tick: self.tick,
            keys_file: self.keys_file.clone(),
            writer: None,
            connection: None,
            patches: patches_tx,
            encoder: ViewEncoder::default(),
            shutdown: shutdown.clone(),
        };
        tokio::spawn(host.run(actions_rx, shutdown));
        (
            Handle {
                actions: actions_tx,
            },
            patches_rx,
        )
    }
}

impl Kv {
    fn open(config: &KvConfig) -> Result<Self, KvError> {
        Ok(match config {
            KvConfig::Memory => Kv::Memory(HashMap::new()),
            KvConfig::Redb(path) => {
                let db = redb::Database::create(path)?;
                // Make sure the table exists so reads never fail on a fresh file.
                let tx = db.begin_write()?;
                tx.open_table(KV_TABLE)?;
                tx.commit()?;
                Kv::Redb(db)
            }
        })
    }

    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, KvError> {
        match self {
            Kv::Memory(m) => Ok(m.get(key).cloned()),
            Kv::Redb(db) => {
                let tx = db.begin_read()?;
                let table = tx.open_table(KV_TABLE)?;
                Ok(table.get(key)?.map(|v| v.value().to_vec()))
            }
        }
    }

    fn put(&mut self, key: &str, value: &[u8]) -> Result<(), KvError> {
        match self {
            Kv::Memory(m) => {
                m.insert(key.to_owned(), value.to_vec());
                Ok(())
            }
            Kv::Redb(db) => {
                let tx = db.begin_write()?;
                tx.open_table(KV_TABLE)?.insert(key, value)?;
                tx.commit()?;
                Ok(())
            }
        }
    }

    fn remove(&mut self, key: &str) -> Result<(), KvError> {
        match self {
            Kv::Memory(m) => {
                m.remove(key);
                Ok(())
            }
            Kv::Redb(db) => {
                let tx = db.begin_write()?;
                tx.open_table(KV_TABLE)?.remove(key)?;
                tx.commit()?;
                Ok(())
            }
        }
    }
}

/// What the UI side holds: send actions in, receive patches out.
#[derive(Debug, Clone)]
pub struct Handle {
    actions: mpsc::UnboundedSender<Command>,
}

/// Commands the UI side sends to the host task.
#[derive(Debug)]
enum Command {
    Dispatch(Action),
    InvalidAction {
        reason: String,
    },
    /// A key chord outside a text input; the core resolves it (§6.4).
    Key(KeyChord),
    /// Emit every section (a UI that just attached).
    Attach,
}

impl Handle {
    /// Queue an action for the core. Returns `false` once the host is gone.
    pub fn dispatch(&self, action: Action) -> bool {
        self.actions.send(Command::Dispatch(action)).is_ok()
    }

    /// Parse untrusted UI input once, retaining validation failures in the active
    /// editor. Both browser and desktop adapters use this same boundary.
    pub fn dispatch_json(&self, action: serde_json::Value) -> bool {
        match serde_json::from_value::<Action>(action) {
            Ok(action) => self.dispatch(action),
            Err(error) => {
                tracing::warn!(%error, "invalid UI action");
                self.actions
                    .send(Command::InvalidAction {
                        reason: error.to_string(),
                    })
                    .is_ok()
            }
        }
    }

    /// Queue a key chord for the core's keymap.
    pub fn key(&self, chord: KeyChord) -> bool {
        self.actions.send(Command::Key(chord)).is_ok()
    }

    /// Ask for every section to be emitted.
    pub fn attach(&self) -> bool {
        self.actions.send(Command::Attach).is_ok()
    }
}

/// Something the transport tasks report to the host task.
#[derive(Debug)]
enum Incoming {
    Connected(mpsc::UnboundedSender<ClientMsg>),
    Msg(ServerMsg),
    Disconnected,
}

/// Start the host on the current runtime. Patches for the UI arrive on the
/// returned receiver, one `Vec` per render; the task ends when `shutdown`
/// is cancelled or the receiver is dropped.
pub fn spawn(
    config: HostConfig,
    shutdown: CancellationToken,
) -> Result<(Handle, mpsc::UnboundedReceiver<ViewDelivery>), HostError> {
    let HostConfig {
        endpoint,
        kv,
        identity,
        cache,
        id_seed,
        tick,
        keys_file,
    } = config;
    let factory = HostFactory::new(HostSettings {
        endpoint,
        kv,
        cache,
        tick,
        keys_file,
    })?;
    Ok(factory.spawn(HostSession { identity, id_seed }, shutdown))
}

struct Host {
    core: ClientCore,
    endpoint: DaemonEndpoint,
    kv: KvStore,
    tick: Duration,
    keys_file: Option<PathBuf>,
    /// Outbound frames while connected.
    writer: Option<mpsc::UnboundedSender<ClientMsg>>,
    /// The dial plus framed connection. Aborted when this host ends so a
    /// connection attempt (especially SSH) cannot outlive its browser tab.
    connection: Option<tokio::task::JoinHandle<()>>,
    patches: mpsc::UnboundedSender<ViewDelivery>,
    encoder: ViewEncoder,
    shutdown: CancellationToken,
}

impl Host {
    fn publish(&mut self, kind: ViewBatchKind, patches: Vec<ViewPatch>) -> bool {
        let result = match self.encoder.encode(kind, patches) {
            Ok(delivery) => self.patches.send(delivery).is_ok(),
            Err(error) => {
                tracing::error!(%error, "view delivery failed; ending host session");
                false
            }
        };
        if !result {
            self.shutdown.cancel();
        }
        result
    }

    /// Unix time in ms: the core stamps ids and pending events with it.
    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
    }

    async fn run(
        mut self,
        mut actions: mpsc::UnboundedReceiver<Command>,
        shutdown: CancellationToken,
    ) {
        let (incoming_tx, mut incoming) = mpsc::unbounded_channel::<Incoming>();
        let mut ticker = tokio::time::interval(self.tick);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                cmd = actions.recv() => match cmd {
                    Some(Command::Dispatch(action)) => self.feed(Input::User(action), &incoming_tx),
                    Some(Command::InvalidAction { reason }) => self.feed(Input::InvalidAction { reason }, &incoming_tx),
                    Some(Command::Key(chord)) => self.feed(Input::Key(chord), &incoming_tx),
                    Some(Command::Attach) => {
                        if !self.publish(ViewBatchKind::Snapshot, self.core.view().full_patches()) {
                            break;
                        }
                    }
                    None => break,
                },
                Some(msg) = incoming.recv() => match msg {
                    Incoming::Connected(writer) => {
                        self.writer = Some(writer);
                        self.feed(Input::Transport(TransportEvent::Connected), &incoming_tx);
                    }
                    Incoming::Msg(msg) => self.feed(Input::Server(msg), &incoming_tx),
                    Incoming::Disconnected => {
                        self.writer = None;
                        self.feed(Input::Transport(TransportEvent::Disconnected), &incoming_tx);
                    }
                },
                _ = ticker.tick() => {
                    let now = Self::now_ms();
                    self.feed(Input::Tick(now), &incoming_tx);
                }
            }
        }
        self.writer = None;
        if let Some(connection) = self.connection.take() {
            connection.abort();
        }
    }

    /// Run one input through the core and act on its effects. Rejections
    /// are ordinary (an unbound key, a stale event): logged, nothing else.
    fn feed(&mut self, input: Input, incoming: &mpsc::UnboundedSender<Incoming>) {
        match self.core.handle(input) {
            Ok(effects) => {
                for e in effects {
                    self.effect(e, incoming);
                }
            }
            Err(err) => tracing::debug!(%err, "input rejected"),
        }
    }

    fn effect(&mut self, effect: Effect, incoming: &mpsc::UnboundedSender<Incoming>) {
        match effect {
            Effect::Connect => {
                if let Some(connection) = self.connection.take() {
                    connection.abort();
                }
                self.connection = Some(tokio::spawn(connect(
                    self.endpoint.clone(),
                    incoming.clone(),
                )));
            }
            Effect::Disconnect => {
                self.writer = None;
                if let Some(connection) = self.connection.take() {
                    connection.abort();
                }
                let _ = incoming.send(Incoming::Disconnected);
            }
            Effect::Send(msg) => {
                if let Some(w) = &self.writer
                    && w.send(msg).is_err()
                {
                    self.writer = None;
                }
            }
            Effect::Persist { key, value } => {
                if let Err(err) = self.kv.put(&key, &value) {
                    tracing::warn!(%err, key, "persist failed");
                }
            }
            Effect::Remove { key } => {
                if let Err(err) = self.kv.remove(&key) {
                    tracing::warn!(%err, key, "remove failed");
                }
            }
            Effect::Load { key } => {
                let value = if key == Keymap::KEY {
                    // keys.toml outranks anything stored: it is the user's
                    // file; a bad file is rejected loudly and whole.
                    self.keymap_overrides()
                } else {
                    match self.kv.get(&key) {
                        Ok(v) => v,
                        Err(err) => {
                            tracing::warn!(%err, key, "load failed");
                            None
                        }
                    }
                };
                // The answer is an input like any other; its effects may
                // load more keys, so recurse through `feed`.
                self.feed(Input::Stored { key, value }, incoming);
            }
            Effect::Render(delta) => {
                let patches = self.core.view().patches(&delta);
                self.publish(ViewBatchKind::Delta, patches);
            }
        }
    }
}

impl Host {
    /// The keymap overrides from `keys.toml`, serialised the way the core
    /// reads them; `None` when there is no file (or it is broken, which is
    /// reported and leaves the defaults).
    fn keymap_overrides(&self) -> Option<Vec<u8>> {
        let path = self.keys_file.as_ref()?;
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
            Err(err) => {
                tracing::error!(%err, path = %path.display(), "keys.toml unreadable; keeping default bindings");
                return None;
            }
        };
        match keys_file::parse(&text) {
            Ok(overrides) => serde_json::to_vec(&overrides).ok(),
            Err(err) => {
                tracing::error!(%err, path = %path.display(), "keys.toml rejected; keeping default bindings");
                None
            }
        }
    }
}

/// Dial the daemon; on success run reader and writer tasks until either
/// side closes. Every outcome reaches the host as an `Incoming`.
async fn connect(endpoint: DaemonEndpoint, incoming: mpsc::UnboundedSender<Incoming>) {
    match nitsd::contexts::dial(&endpoint).await {
        Ok(connection) => {
            let (read, write) = connection.into_parts();
            serve_framed(read, write, incoming).await;
        }
        Err(err) => {
            tracing::warn!(%err, ?endpoint, "connect failed");
            let _ = incoming.send(Incoming::Disconnected);
        }
    }
}

async fn serve_framed<R, W>(mut read: R, mut write: W, incoming: mpsc::UnboundedSender<Incoming>)
where
    R: FrameRead + 'static,
    W: FrameWrite + 'static,
{
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<ClientMsg>();
    if incoming.send(Incoming::Connected(out_tx)).is_err() {
        return;
    }
    // A byte-frame read is not cancellation-safe: dropping `recv_msg` after
    // it consumed part of a length or payload would desynchronise the next
    // read. Keep each direction in its own task. `JoinSet` aborts both tasks
    // if this owning connection future is itself cancelled.
    let mut sides = tokio::task::JoinSet::new();
    sides.spawn(async move {
        while let Some(msg) = out_rx.recv().await {
            if transport::send_msg(&mut write, &Envelope::current(msg))
                .await
                .is_err()
            {
                return;
            }
        }
        let _ = write.close().await;
    });
    let reader_incoming = incoming.clone();
    sides.spawn(async move {
        loop {
            match transport::recv_msg::<_, ServerMsg>(&mut read).await {
                Ok(Some(env)) => {
                    if reader_incoming.send(Incoming::Msg(env.msg)).is_err() {
                        return;
                    }
                }
                Ok(None) => return,
                Err(err) => {
                    tracing::debug!(%err, "read failed");
                    return;
                }
            }
        }
    });
    let _ = sides.join_next().await;
    sides.abort_all();
    while sides.join_next().await.is_some() {}
    let _ = incoming.send(Incoming::Disconnected);
}

/// Sensible defaults for a native UI host: 100 ms ticks and the user's key
/// overrides. The caller chooses the KV tier independently of the endpoint.
#[must_use]
pub fn host_config(
    endpoint: DaemonEndpoint,
    identity: Identity,
    id_seed: IdSeed,
    kv: KvConfig,
) -> HostConfig {
    let cache = CacheConfig {
        disk: match (&endpoint, &kv) {
            (DaemonEndpoint::Ssh { .. } | DaemonEndpoint::WebSocket { .. }, KvConfig::Redb(_)) => {
                DiskTier::Enabled {
                    budget: Bytes::mib(2048),
                }
            }
            (DaemonEndpoint::Local { .. }, KvConfig::Memory | KvConfig::Redb(_))
            | (DaemonEndpoint::Ssh { .. } | DaemonEndpoint::WebSocket { .. }, KvConfig::Memory) => {
                DiskTier::Disabled
            }
        },
        ..CacheConfig::default()
    };
    HostConfig {
        endpoint,
        kv,
        identity,
        cache,
        id_seed,
        tick: Duration::from_millis(100),
        keys_file: keys_file::default_keys_path(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nitsd::contexts::{StartPolicy, local_spec};
    use tokio::io::AsyncWriteExt as _;

    fn identity() -> Identity {
        Identity {
            client_id: ClientId::from_parts(1, 1),
            client: BuildInfo {
                name: "host-test".into(),
                version: "0".into(),
            },
            author: Author::Human {
                name: "ada".into(),
                machine: "box".into(),
            },
        }
    }

    #[test]
    fn persistent_remote_hosts_get_the_documented_disk_tier() {
        let remote = host_config(
            DaemonEndpoint::WebSocket {
                url: "ws://review.example:7677".into(),
            },
            identity(),
            IdSeed(1),
            KvConfig::Redb(PathBuf::from("remote.redb")),
        );
        assert_eq!(
            remote.cache.disk,
            DiskTier::Enabled {
                budget: Bytes::mib(2048)
            }
        );
    }

    #[test]
    fn local_or_memory_only_hosts_do_not_duplicate_the_disk_tier() {
        let local = host_config(
            DaemonEndpoint::Local {
                spec: local_spec(
                    Some(&PathBuf::from("/tmp/nits-host-test")),
                    Some(&PathBuf::from("/tmp/nits-host-test.sock")),
                )
                .unwrap(),
                start: StartPolicy::StartIfNeeded,
            },
            identity(),
            IdSeed(1),
            KvConfig::Redb(PathBuf::from("local.redb")),
        );
        let memory_remote = host_config(
            DaemonEndpoint::WebSocket {
                url: "ws://review.example:7677".into(),
            },
            identity(),
            IdSeed(2),
            KvConfig::Memory,
        );
        assert_eq!(local.cache.disk, DiskTier::Disabled);
        assert_eq!(memory_remote.cache.disk, DiskTier::Disabled);
    }

    #[tokio::test]
    async fn outbound_work_does_not_cancel_a_fragmented_inbound_frame() {
        let (host, mut peer) = tokio::io::duplex(4096);
        let (read, write) = transport::byte_stream(host);
        let (incoming, mut received) = mpsc::unbounded_channel();
        let connection = tokio::spawn(serve_framed(read, write, incoming));
        let out = match received.recv().await.unwrap() {
            Incoming::Connected(out) => out,
            Incoming::Msg(_) | Incoming::Disconnected => panic!("connection did not start"),
        };

        let payload = serde_json::to_vec(&Envelope::current(ServerMsg::Rejected {
            error: nits_protocol::RpcError::Cancelled,
        }))
        .unwrap();
        let mut frame = u32::try_from(payload.len()).unwrap().to_be_bytes().to_vec();
        frame.extend(payload);
        peer.write_all(&frame[..2]).await.unwrap();
        tokio::task::yield_now().await;
        out.send(ClientMsg::Request {
            id: nits_protocol::RequestId::new(1),
            request: nits_protocol::Request::Shutdown,
        })
        .unwrap();
        tokio::task::yield_now().await;
        peer.write_all(&frame[2..]).await.unwrap();

        let inbound = tokio::time::timeout(Duration::from_secs(1), received.recv())
            .await
            .expect("fragmented frame timed out")
            .expect("connection ended during fragmented frame");
        assert!(matches!(
            inbound,
            Incoming::Msg(ServerMsg::Rejected {
                error: nits_protocol::RpcError::Cancelled
            })
        ));
        drop(peer);
        connection.await.unwrap();
    }
}
