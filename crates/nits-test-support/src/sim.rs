//! Two-client simulator (plan 3.3): N `ClientCore`s and an in-memory daemon
//! model joined by per-client FIFO queues whose delivery the test controls
//! one message at a time. No I/O, no clock, no threads.
//!
//! The daemon model reuses `nits_client_core::{local_event, apply_event}`
//! for its own state, so "converged" means the client's optimistic
//! semantics and the daemon's agree — which is exactly what §5.2 promises.

use std::collections::VecDeque;

use nits_client_core::{
    Action, CacheConfig, ClientCore, Config, CoreError, Effect, EventMeta, IdSeed, Input,
    MutationError, TransportEvent, apply_event, local_event,
};
use nits_protocol::{
    Author, BuildInfo, ClientId, ClientMsg, EntityKind, Event, ProtocolVersion, Request, RequestId,
    Response, ReviewSnapshot, RpcError, SchemaVersion, Seq, ServerMsg, Since, StreamItem,
    Timestamp,
};

/// Index of a simulated client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Peer(pub usize);

/// What the daemon model knows about one connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Session {
    Down,
    /// Transport up, `Hello` not yet processed.
    Connected,
    /// Handshake done; `subscribed` once a `Subscribe` was answered.
    Live {
        subscribed: bool,
    },
}

#[derive(Debug)]
struct Client {
    core: ClientCore,
    author: Author,
    /// Client → daemon, undelivered.
    up: VecDeque<ClientMsg>,
    /// Daemon → client, undelivered.
    down: VecDeque<ServerMsg>,
    session: Session,
}

/// The daemon as the clients see it: one review, an event log, a clock.
#[derive(Debug)]
struct DaemonModel {
    snapshot: ReviewSnapshot,
    log: Vec<Event>,
    now_ms: i64,
    trees: Vec<(nits_protocol::RefSpec, nits_protocol::TreeSnapshot)>,
    blobs: Vec<nits_protocol::FileRender>,
}

impl DaemonModel {
    fn last_seq(&self) -> Seq {
        self.log.last().map_or(self.snapshot.seq, |e| e.seq)
    }

    fn next_seq(&self) -> Seq {
        Seq::new(self.last_seq().get() + 1)
    }
}

/// How a client diverged from the daemon after everything was delivered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Divergence {
    #[error("client {0:?} still has {1} pending mutation(s)")]
    Pending(Peer, usize),
    #[error("client {0:?} comments differ from the daemon's")]
    Comments(Peer),
    #[error("client {0:?} threads differ from the daemon's")]
    Threads(Peer),
    #[error("client {0:?} requests differ from the daemon's")]
    Requests(Peer),
    #[error("client {0:?} checkpoints differ from the daemon's")]
    Checkpoints(Peer),
    #[error("client {0:?} has no review open")]
    NotOpen(Peer),
}

/// See the module docs.
#[derive(Debug)]
pub struct Sim {
    daemon: DaemonModel,
    clients: Vec<Client>,
}

impl Sim {
    /// A daemon holding `snapshot` and one disconnected client per author.
    #[must_use]
    pub fn new(snapshot: ReviewSnapshot, authors: Vec<Author>) -> Self {
        let clients = authors
            .into_iter()
            .enumerate()
            .map(|(i, author)| {
                let n = u128::try_from(i + 1).unwrap_or(1);
                let core = ClientCore::new(Config {
                    client_id: ClientId::from_parts(1, n),
                    client: BuildInfo {
                        name: "sim".into(),
                        version: "0".into(),
                    },
                    author: author.clone(),
                    id_seed: IdSeed(0x5eed + n),
                    cache: CacheConfig::default(),
                });
                Client {
                    core,
                    author,
                    up: VecDeque::new(),
                    down: VecDeque::new(),
                    session: Session::Down,
                }
            })
            .collect();
        Self {
            daemon: DaemonModel {
                snapshot,
                log: Vec::new(),
                now_ms: 1_000,
                trees: Vec::new(),
                blobs: Vec::new(),
            },
            clients,
        }
    }

    /// Supply immutable Browse content served by the simulated daemon.
    pub fn browse_content(
        &mut self,
        reference: nits_protocol::RefSpec,
        tree: nits_protocol::TreeSnapshot,
        blob: nits_protocol::FileRender,
    ) {
        self.daemon.trees.push((reference, tree));
        self.daemon.blobs.push(blob);
    }

    #[must_use]
    pub fn client(&self, peer: Peer) -> &ClientCore {
        &self.clients[peer.0].core
    }

    #[must_use]
    pub fn daemon_snapshot(&self) -> &ReviewSnapshot {
        &self.daemon.snapshot
    }

    #[must_use]
    pub fn log(&self) -> &[Event] {
        &self.daemon.log
    }

    #[must_use]
    pub fn peers(&self) -> Vec<Peer> {
        (0..self.clients.len()).map(Peer).collect()
    }

    /// Commit an external actor's review invitation and queue live delivery.
    pub fn request_review(&mut self, requester: Author, recipient: String, note: String) -> Event {
        let event = Event {
            seq: self.daemon.next_seq(),
            ts: Timestamp::from_millis(self.daemon.now_ms),
            author: requester,
            client_id: ClientId::from_parts(1, u128::MAX),
            client_seq: nits_protocol::ClientSeq::new(self.daemon.next_seq().get()),
            body: nits_protocol::EventBody::ReviewRequested {
                review_id: self.daemon.snapshot.review.id,
                agent: recipient,
                note,
                targets: self
                    .daemon
                    .snapshot
                    .resolved
                    .clone()
                    .map_or(nits_protocol::RequestedTargets::Unknown, |targets| {
                        nits_protocol::RequestedTargets::Captured { targets }
                    }),
            },
        };
        apply_event(&mut self.daemon.snapshot, &event);
        self.daemon.log.push(event.clone());
        for i in 0..self.clients.len() {
            if self.clients[i].session == (Session::Live { subscribed: true }) {
                self.push_down(
                    Peer(i),
                    ServerMsg::Event {
                        event: event.clone(),
                    },
                );
            }
        }
        event
    }

    /// Commit a moving-target update independently of either reviewer.
    pub fn advance_targets(
        &mut self,
        targets: nits_protocol::NonEmpty<nits_protocol::ResolvedTarget>,
    ) {
        let event = Event {
            seq: self.daemon.next_seq(),
            ts: Timestamp::from_millis(self.daemon.now_ms),
            author: Author::Daemon {
                machine: "sim".into(),
            },
            client_id: ClientId::from_parts(1, u128::MAX),
            client_seq: nits_protocol::ClientSeq::new(self.daemon.next_seq().get()),
            body: nits_protocol::EventBody::ReviewTargetsResolved {
                review_id: self.daemon.snapshot.review.id,
                targets,
            },
        };
        apply_event(&mut self.daemon.snapshot, &event);
        self.daemon.log.push(event.clone());
        for i in 0..self.clients.len() {
            if self.clients[i].session == (Session::Live { subscribed: true }) {
                self.push_down(
                    Peer(i),
                    ServerMsg::Event {
                        event: event.clone(),
                    },
                );
            }
        }
    }

    /// Messages waiting in each direction for `peer`.
    #[must_use]
    pub fn queued(&self, peer: Peer) -> (usize, usize) {
        let c = &self.clients[peer.0];
        (c.up.len(), c.down.len())
    }

    /// Advance every clock (client ticks and the daemon's) by `ms`.
    pub fn tick(&mut self, ms: u64) {
        self.daemon.now_ms += i64::try_from(ms).unwrap_or(i64::MAX);
        let now = u64::try_from(self.daemon.now_ms).unwrap_or(u64::MAX);
        for i in 0..self.clients.len() {
            // Ticks never fail and produce no effects.
            let _ = self.clients[i].core.handle(Input::Tick(now));
        }
    }

    /// Drive one client's input; its effects are routed into the queues.
    pub fn act(&mut self, peer: Peer, action: Action) -> Result<Vec<Effect>, CoreError> {
        let effects = self.clients[peer.0].core.handle(Input::User(action))?;
        self.route(peer, &effects);
        Ok(effects)
    }

    /// Connect, handshake and open the review, delivering everything.
    /// The client ends `Subscribed` with the review open.
    pub fn connect_and_open(&mut self, peer: Peer) -> Result<(), CoreError> {
        self.act(peer, Action::Connect)?;
        self.settle();
        let review_id = self.daemon.snapshot.review.id;
        self.act(peer, Action::OpenReview { review_id })?;
        self.settle();
        Ok(())
    }

    /// Drop the transport: queued messages in both directions are lost.
    pub fn disconnect(&mut self, peer: Peer) {
        let c = &mut self.clients[peer.0];
        c.up.clear();
        c.down.clear();
        c.session = Session::Down;
        // A core that is already down ignores this.
        let _ = c
            .core
            .handle(Input::Transport(TransportEvent::Disconnected));
    }

    /// Dial again. The handshake and resubscribe are queued, not delivered.
    pub fn reconnect(&mut self, peer: Peer) -> Result<(), CoreError> {
        self.act(peer, Action::Connect)?;
        Ok(())
    }

    /// Deliver one message client → daemon. `false` if nothing was queued.
    pub fn deliver_up(&mut self, peer: Peer) -> bool {
        let Some(msg) = self.clients[peer.0].up.pop_front() else {
            return false;
        };
        self.daemon_handle(peer, msg);
        true
    }

    /// Deliver one message daemon → client. `None` if nothing was queued;
    /// otherwise the core's verdict on it.
    pub fn deliver_down(&mut self, peer: Peer) -> Option<Result<Vec<Effect>, CoreError>> {
        let msg = self.clients[peer.0].down.pop_front()?;
        let result = self.clients[peer.0].core.handle(Input::Server(msg));
        if let Ok(effects) = &result {
            self.route(peer, effects);
        }
        Some(result)
    }

    /// Deliver everything, round-robin, until every queue is empty.
    pub fn settle(&mut self) {
        loop {
            let mut moved = false;
            for i in 0..self.clients.len() {
                moved |= self.deliver_up(Peer(i));
                moved |= self.deliver_down(Peer(i)).is_some();
            }
            if !moved {
                break;
            }
        }
    }

    /// Every client with the review open shows exactly the daemon's state
    /// and has nothing pending.
    pub fn converged(&self) -> Result<(), Divergence> {
        for (i, c) in self.clients.iter().enumerate() {
            let peer = Peer(i);
            let Some(open) = &c.core.view().review else {
                return Err(Divergence::NotOpen(peer));
            };
            if !open.pending.is_empty() {
                return Err(Divergence::Pending(peer, open.pending.len()));
            }
            if open.snapshot.comments != self.daemon.snapshot.comments {
                return Err(Divergence::Comments(peer));
            }
            if open.snapshot.threads != self.daemon.snapshot.threads {
                return Err(Divergence::Threads(peer));
            }
            if open.snapshot.checkpoints != self.daemon.snapshot.checkpoints {
                return Err(Divergence::Checkpoints(peer));
            }
            if open.snapshot.requests != self.daemon.snapshot.requests {
                return Err(Divergence::Requests(peer));
            }
        }
        Ok(())
    }

    // ---- routing ----------------------------------------------------------

    fn route(&mut self, peer: Peer, effects: &[Effect]) {
        for e in effects {
            match e {
                Effect::Connect => {
                    // The transport comes up at once; the Hello goes out
                    // through the queue like any other message.
                    let c = &mut self.clients[peer.0];
                    c.session = Session::Connected;
                    if let Ok(more) = c.core.handle(Input::Transport(TransportEvent::Connected)) {
                        self.route(peer, &more);
                    }
                }
                Effect::Disconnect => self.disconnect(peer),
                Effect::Send(msg) => self.clients[peer.0].up.push_back(msg.clone()),
                Effect::Render(_)
                | Effect::Persist { .. }
                | Effect::Load { .. }
                | Effect::Remove { .. } => {}
            }
        }
    }

    fn push_down(&mut self, peer: Peer, msg: ServerMsg) {
        self.clients[peer.0].down.push_back(msg);
    }

    fn daemon_handle(&mut self, peer: Peer, msg: ClientMsg) {
        let session = self.clients[peer.0].session;
        match msg {
            ClientMsg::Hello { .. } => {
                if session == Session::Connected {
                    self.clients[peer.0].session = Session::Live { subscribed: false };
                    self.push_down(
                        peer,
                        ServerMsg::Welcome {
                            protocol: ProtocolVersion::CURRENT,
                            daemon: BuildInfo {
                                name: "sim-daemon".into(),
                                version: "0".into(),
                            },
                            schema: SchemaVersion::CURRENT,
                            upgrade: None,
                        },
                    );
                }
            }
            ClientMsg::Cancel { .. } => {}
            ClientMsg::Request { id, request } => {
                if !matches!(session, Session::Live { .. }) {
                    return;
                }
                self.daemon_request(peer, id, request);
            }
        }
    }

    // One arm per request; splitting would hide the exhaustive match.
    #[allow(clippy::too_many_lines)]
    fn daemon_request(&mut self, peer: Peer, id: RequestId, request: Request) {
        match request {
            Request::Subscribe { since, .. } => {
                // Like `nitsd`: replay the gap first, then answer with the
                // head position live events continue from.
                let seq = self.daemon.last_seq();
                let replay: Vec<Event> = match since {
                    Since::Now => Vec::new(),
                    Since::After { seq } => self
                        .daemon
                        .log
                        .iter()
                        .filter(|e| e.seq > seq)
                        .cloned()
                        .collect(),
                };
                for event in replay {
                    self.push_down(peer, ServerMsg::Event { event });
                }
                self.push_down(
                    peer,
                    ServerMsg::Response {
                        id,
                        response: Response::Subscribed { seq },
                    },
                );
                self.clients[peer.0].session = Session::Live { subscribed: true };
            }
            Request::OpenReview { review_id, .. } | Request::ReviewSnapshot { review_id } => {
                if review_id != self.daemon.snapshot.review.id {
                    self.push_down(
                        peer,
                        ServerMsg::Error {
                            id,
                            error: RpcError::NotFound {
                                kind: EntityKind::Review,
                                id: review_id.to_string(),
                            },
                        },
                    );
                    return;
                }
                let snapshot = self.daemon.snapshot.clone();
                match request {
                    Request::OpenReview { .. } => {
                        self.push_down(
                            peer,
                            ServerMsg::StreamItem {
                                id,
                                item: StreamItem::ReviewSnapshot { snapshot },
                            },
                        );
                        self.push_down(peer, ServerMsg::StreamEnd { id });
                    }
                    Request::ReviewSnapshot { .. } => self.push_down(
                        peer,
                        ServerMsg::Response {
                            id,
                            response: Response::ReviewSnapshot { snapshot },
                        },
                    ),
                    Request::ListWorkspaces
                    | Request::ListReviews { .. }
                    | Request::ListRefs { .. }
                    | Request::DefaultBase { .. }
                    | Request::EnsureDirectoryReview { .. }
                    | Request::GetReview { .. }
                    | Request::ListFiles { .. }
                    | Request::ResolveTargets { .. }
                    | Request::ListCommits { .. }
                    | Request::TreeSnapshot { .. }
                    | Request::Search { .. }
                    | Request::FileRender { .. }
                    | Request::ChangeRender { .. }
                    | Request::BlobRender { .. }
                    | Request::RenderChunk { .. }
                    | Request::Subscribe { .. }
                    | Request::ReplayEvents { .. }
                    | Request::Unsubscribe { .. }
                    | Request::Mutate { .. }
                    | Request::Shutdown => {}
                }
            }
            Request::Mutate {
                client_seq,
                mutation,
            } => {
                let author = self.clients[peer.0].author.clone();
                let meta = EventMeta {
                    author: author.clone(),
                    ts: Timestamp::from_millis(self.daemon.now_ms),
                };
                let body = if let nits_protocol::Mutation::RecordCheckpoint {
                    review_id,
                    targets,
                    in_reply_to,
                } = &mutation
                {
                    nits_protocol::ReviewerIdentity::from_author(&author)
                        .map(|reviewer| nits_protocol::EventBody::ReviewChecked {
                            review_id: *review_id,
                            reviewer,
                            targets: targets.clone(),
                            in_reply_to: *in_reply_to,
                        })
                        .ok_or(MutationError::Unsupported(
                            nits_protocol::MutationKind::RecordCheckpoint,
                        ))
                } else {
                    local_event(&self.daemon.snapshot, &meta, &mutation)
                };
                match body {
                    Ok(body) => {
                        let event = Event {
                            seq: self.daemon.next_seq(),
                            ts: meta.ts,
                            author,
                            client_id: self.clients[peer.0].core.client_id(),
                            client_seq,
                            body,
                        };
                        apply_event(&mut self.daemon.snapshot, &event);
                        self.daemon.log.push(event.clone());
                        self.push_down(
                            peer,
                            ServerMsg::Response {
                                id,
                                response: Response::Committed {
                                    event: event.clone(),
                                },
                            },
                        );
                        for i in 0..self.clients.len() {
                            if self.clients[i].session == (Session::Live { subscribed: true }) {
                                self.push_down(
                                    Peer(i),
                                    ServerMsg::Event {
                                        event: event.clone(),
                                    },
                                );
                            }
                        }
                    }
                    Err(e) => self.push_down(
                        peer,
                        ServerMsg::Error {
                            id,
                            error: rpc_error(&e),
                        },
                    ),
                }
            }
            Request::ListFiles { .. } => self.push_down(
                peer,
                ServerMsg::Response {
                    id,
                    response: Response::Files {
                        files: Vec::new(),
                        resolved: self
                            .daemon
                            .snapshot
                            .resolved
                            .clone()
                            .map_or_else(Vec::new, |targets| targets.into_iter().collect()),
                    },
                },
            ),
            Request::ListWorkspaces => self.push_down(
                peer,
                ServerMsg::Response {
                    id,
                    response: Response::Workspaces {
                        workspaces: Vec::new(),
                    },
                },
            ),
            Request::ListRefs { repo_id } => self.push_down(
                peer,
                ServerMsg::Response {
                    id,
                    response: Response::Refs {
                        repo_id,
                        refs: vec![nits_protocol::RefCandidate {
                            ref_spec: nits_protocol::RefSpec::WorkingTree,
                            subject: None,
                        }],
                    },
                },
            ),
            Request::TreeSnapshot { repo_id, ref_spec } => {
                if let Some((_, snapshot)) = self
                    .daemon
                    .trees
                    .iter()
                    .find(|(reference, tree)| *reference == ref_spec && tree.repo_id == repo_id)
                    .cloned()
                {
                    self.push_down(
                        peer,
                        ServerMsg::Response {
                            id,
                            response: Response::TreeSnapshot { snapshot },
                        },
                    );
                } else {
                    self.push_down(
                        peer,
                        ServerMsg::Error {
                            id,
                            error: RpcError::Invalid {
                                reason: "tree not supplied to simulator".into(),
                            },
                        },
                    );
                }
            }
            Request::BlobRender {
                repo_id,
                path,
                entry,
                first_chunk,
            } => {
                if let Some(render) = self
                    .daemon
                    .blobs
                    .iter()
                    .find(|r| {
                        r.header.repo_id == repo_id
                            && r.header.path == path
                            && r.header.target == (nits_protocol::RenderTarget::Blob { entry })
                    })
                    .cloned()
                {
                    self.push_down(
                        peer,
                        ServerMsg::StreamItem {
                            id,
                            item: StreamItem::Header {
                                header: render.header,
                            },
                        },
                    );
                    for chunk in render.chunks.into_iter().filter(|c| c.index >= first_chunk) {
                        self.push_down(
                            peer,
                            ServerMsg::StreamItem {
                                id,
                                item: StreamItem::Chunk {
                                    repo_id,
                                    path: path.clone(),
                                    chunk,
                                },
                            },
                        );
                    }
                    self.push_down(peer, ServerMsg::StreamEnd { id });
                } else {
                    self.push_down(
                        peer,
                        ServerMsg::Error {
                            id,
                            error: RpcError::Invalid {
                                reason: "blob not supplied to simulator".into(),
                            },
                        },
                    );
                }
            }
            Request::ListReviews { .. }
            | Request::DefaultBase { .. }
            | Request::EnsureDirectoryReview { .. }
            | Request::GetReview { .. }
            | Request::ResolveTargets { .. }
            | Request::ListCommits { .. }
            | Request::Search { .. }
            | Request::FileRender { .. }
            | Request::ChangeRender { .. }
            | Request::RenderChunk { .. }
            | Request::Unsubscribe { .. }
            | Request::ReplayEvents { .. }
            | Request::Shutdown => self.push_down(
                peer,
                ServerMsg::Error {
                    id,
                    error: RpcError::Internal {
                        message: "not modelled by the simulator".into(),
                    },
                },
            ),
        }
    }
}

/// The wire error the real daemon would answer with.
fn rpc_error(e: &MutationError) -> RpcError {
    match e {
        MutationError::InformationalAnchor
        | MutationError::InformationalThread(_)
        | MutationError::DuplicateComment(_)
        | MutationError::AlreadyResolved(_)
        | MutationError::NotResolved(_)
        | MutationError::NotOpen(_)
        | MutationError::Unsupported(_) => RpcError::Invalid {
            reason: e.to_string(),
        },
        MutationError::UnknownThread(id) => RpcError::NotFound {
            kind: EntityKind::Thread,
            id: id.to_string(),
        },
        MutationError::UnknownComment(id) => RpcError::NotFound {
            kind: EntityKind::Comment,
            id: id.to_string(),
        },
        MutationError::UnknownRepo(id) => RpcError::NotFound {
            kind: EntityKind::Repo,
            id: id.to_string(),
        },
        MutationError::NotAuthor(_) => RpcError::Forbidden {
            reason: e.to_string(),
        },
    }
}
