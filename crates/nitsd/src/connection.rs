//! One client connection: handshake, request multiplexing, cancellation,
//! subscriptions and streamed responses.
//!
//! Requests are polled concurrently so a long render never delays a mutation
//! from the same client. Outgoing frames funnel through one unbounded channel;
//! ordering within a request is preserved because each request sends sequentially.
//! The serving future owns every request, subscription tail and transport half,
//! so dropping it cannot detach work that keeps a connection or daemon alive.

use std::collections::HashMap;
use std::sync::Arc;

use futures_util::{StreamExt, stream::FuturesUnordered};
use nits_protocol::{
    ChunkIndex, ClientMsg, Envelope, Event, ProtocolVersion, RenderChunk, Request, RequestId,
    Response, ResponseShape, RpcError, Seq, ServerMsg, Since, StreamItem, SubscribeScope,
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::codec::CodecError;
use crate::daemon::{Daemon, DaemonError};
use crate::dispatch;
use crate::handshake::{AwaitingHello, Negotiated};
use crate::transport::{self, FrameRead, FrameWrite};

#[derive(Debug, thiserror::Error)]
pub enum ConnectionError {
    #[error(transparent)]
    Codec(#[from] CodecError),
    #[error("handshake rejected")]
    Rejected,
}

/// Outgoing side of a connection, cheap to clone into concurrent requests.
#[derive(Debug, Clone)]
pub struct Outbox {
    tx: mpsc::UnboundedSender<ServerMsg>,
}

impl Outbox {
    /// Queue a message. A closed connection drops it; its request futures are
    /// dropped along with the serving future.
    pub fn send(&self, msg: ServerMsg) {
        let _ = self.tx.send(msg);
    }
}

/// Per-connection subscription state, shared with the event tail.
#[derive(Debug, Default)]
struct Subscriptions {
    /// Live-tail watermark per scope. Explicit replay ignores prior delivery;
    /// this only prevents the tail from repeating that subscription's replay.
    scopes: HashMap<SubscribeScope, Seq>,
}

/// Serve one already-accepted byte stream (length-prefixed frames) until
/// the peer disconnects.
pub async fn serve<S>(daemon: Arc<Daemon>, stream: S) -> Result<(), ConnectionError>
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let (rd, wr) = transport::byte_stream(stream);
    serve_framed(daemon, rd, wr).await
}

/// Serve one connection over any framed transport.
pub async fn serve_framed<R, W>(
    daemon: Arc<Daemon>,
    mut rd: R,
    mut wr: W,
) -> Result<(), ConnectionError>
where
    R: FrameRead + 'static,
    W: FrameWrite + 'static,
{
    // Handshake happens inline: nothing else may be in flight yet.
    let Some(first) = transport::recv_msg::<_, ClientMsg>(&mut rd).await? else {
        return Ok(());
    };
    let hello = AwaitingHello {
        daemon: daemon.build.clone(),
        schema: daemon.schema(),
    };
    let negotiated = match hello.negotiate(first) {
        Ok((n, welcome)) => {
            transport::send_msg(&mut wr, &welcome).await?;
            n
        }
        Err(rejected) => {
            transport::send_msg(&mut wr, &rejected.reply).await?;
            wr.close().await.ok();
            return Err(ConnectionError::Rejected);
        }
    };
    tracing::info!(client = %negotiated.client_id, name = %negotiated.client.name, "connected");

    let (tx, rx) = mpsc::unbounded_channel();
    let outbox = Outbox { tx };
    let writer = write_loop(wr, rx, negotiated.protocol);
    tokio::pin!(writer);

    let _tracked = daemon.track_connection();
    let conn = Arc::new(Connection {
        daemon,
        negotiated,
        outbox: outbox.clone(),
        subs: Mutex::new(Subscriptions::default()),
    });
    // Start observing broadcasts before any subscription request can run.
    let events = conn.daemon.subscribe();
    let result = {
        let incoming = async {
            tokio::select! {
                result = read_loop(&conn, &mut rd) => result,
                () = event_tail(Arc::clone(&conn), events) => Ok(()),
                () = delta_tail(Arc::clone(&conn)) => Ok(()),
            }
        };
        tokio::select! {
            result = incoming => result,
            result = &mut writer => return result.map_err(ConnectionError::from),
        }
    };

    // All producers have now been dropped, including pending requests. Clean
    // EOF drains already queued frames in order. A malformed/failed connection
    // closes immediately instead of waiting for a peer that may never read.
    drop(conn);
    drop(outbox);
    result?;
    writer.await?;
    Ok(())
}

struct Connection {
    daemon: Arc<Daemon>,
    negotiated: Negotiated,
    outbox: Outbox,
    subs: Mutex<Subscriptions>,
}

async fn read_loop<R: FrameRead>(
    conn: &Arc<Connection>,
    rd: &mut R,
) -> Result<(), ConnectionError> {
    let mut requests = FuturesUnordered::new();
    let mut in_flight: HashMap<RequestId, Arc<CancellationToken>> = HashMap::new();
    loop {
        // Reading a length-prefixed frame is not cancellation-safe. Keep this
        // future alive while completed requests are removed from the set.
        let next = transport::recv_msg::<_, ClientMsg>(rd);
        tokio::pin!(next);
        let env = loop {
            tokio::select! {
                env = &mut next => break env?,
                Some((id, token)) = requests.next(), if !requests.is_empty() => {
                    // An older completion must not remove a reused request ID.
                    if in_flight.get(&id).is_some_and(|current| Arc::ptr_eq(current, &token)) {
                        in_flight.remove(&id);
                    }
                }
            }
        };
        let Some(env) = env else {
            return Ok(());
        };
        if let Err(error) = conn.negotiated.check(env.v) {
            // No request id to attach it to for a bad Hello re-send; use 0.
            let id = match &env.msg {
                ClientMsg::Request { id, .. } | ClientMsg::Cancel { id } => *id,
                ClientMsg::Hello { .. } => RequestId::new(0),
            };
            conn.outbox.send(ServerMsg::Error { id, error });
            continue;
        }
        match env.msg {
            ClientMsg::Hello { .. } => {
                conn.outbox.send(ServerMsg::Error {
                    id: RequestId::new(0),
                    error: RpcError::Invalid {
                        reason: "Hello after handshake".into(),
                    },
                });
            }
            ClientMsg::Request { id, request } => {
                let c = Arc::clone(conn);
                let token = Arc::new(CancellationToken::new());
                in_flight.insert(id, Arc::clone(&token));
                requests.push(async move {
                    tokio::select! {
                        biased;
                        () = token.cancelled() => {},
                        () = c.handle(id, request) => {},
                    }
                    (id, token)
                });
            }
            ClientMsg::Cancel { id } => {
                if let Some(token) = in_flight.remove(&id) {
                    token.cancel();
                    conn.outbox.send(ServerMsg::Error {
                        id,
                        error: RpcError::Cancelled,
                    });
                    conn.outbox.send(ServerMsg::StreamEnd { id });
                }
            }
        }
    }
}

async fn write_loop<W: FrameWrite>(
    mut wr: W,
    mut rx: mpsc::UnboundedReceiver<ServerMsg>,
    v: ProtocolVersion,
) -> Result<(), CodecError> {
    while let Some(msg) = rx.recv().await {
        let env = Envelope { v, msg };
        if let Err(e) = transport::send_msg(&mut wr, &env).await {
            tracing::debug!(error = %e, "write failed; closing");
            return Err(e);
        }
    }
    wr.close().await
}

/// Forward broadcast events matching this connection's scopes.
async fn event_tail(conn: Arc<Connection>, mut rx: tokio::sync::broadcast::Receiver<Arc<Event>>) {
    loop {
        match rx.recv().await {
            Ok(event) => conn.deliver_live(&event).await,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                // The client fell behind the backlog. Tell it where we are
                // so it can resubscribe from its last seen seq.
                tracing::warn!(skipped = n, "subscriber lagged");
                let oldest = conn
                    .daemon
                    .read(nits_review_core::Core::last_seq)
                    .await
                    .ok()
                    .flatten()
                    .unwrap_or(Seq::FIRST);
                conn.outbox.send(ServerMsg::Error {
                    id: RequestId::new(0),
                    error: RpcError::SeqTooOld { oldest },
                });
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// Forward working-tree deltas matching this connection's scopes.
async fn delta_tail(conn: Arc<Connection>) {
    let mut rx = conn.daemon.subscribe_deltas();
    loop {
        match rx.recv().await {
            Ok(delta) => {
                let subs = conn.subs.lock().await;
                if subs
                    .scopes
                    .keys()
                    .any(|s| conn.daemon.delta_matches(s, &delta))
                {
                    conn.outbox.send(ServerMsg::TreeDelta {
                        delta: (*delta).clone(),
                    });
                }
            }
            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                tracing::warn!(skipped = n, "delta subscriber lagged");
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
    }
}

impl Connection {
    async fn deliver_live(&self, event: &Arc<Event>) {
        let mut subs = self.subs.lock().await;
        let mut deliver = false;
        for (scope, delivered) in &mut subs.scopes {
            if event.seq > *delivered && self.daemon.matches(scope, event) {
                *delivered = event.seq;
                deliver = true;
            }
        }
        if deliver {
            self.outbox.send(ServerMsg::Event {
                event: (**event).clone(),
            });
        }
    }

    async fn handle(self: Arc<Self>, id: RequestId, request: Request) {
        let shape = request.shape();
        let result = match request {
            Request::Subscribe { scope, since } => self.subscribe(scope, since).await,
            Request::Unsubscribe { scope } => {
                self.subs.lock().await.scopes.remove(&scope);
                Ok(Response::Unsubscribed)
            }
            Request::Shutdown => {
                tracing::info!(client = %self.negotiated.client_id, "shutdown requested");
                let token = self.daemon.shutdown().clone();
                // Let the reply reach the client before the accept loop
                // drops every connection.
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    token.cancel();
                });
                Ok(Response::ShuttingDown)
            }
            Request::OpenReview { review_id, opts } => {
                let r =
                    dispatch::open_review(&self.daemon, id, &self.outbox, review_id, opts).await;
                self.finish_stream(id, r);
                return;
            }
            Request::FileRender {
                review_id,
                repo_id,
                path,
                opts,
                first_chunk,
                scope,
            } => {
                let r = self
                    .daemon
                    .read(move |core| core.file_render(review_id, repo_id, &path, opts, &scope))
                    .await
                    .map(|(header, rendered)| {
                        stream_render(&self.outbox, id, header, &rendered, first_chunk);
                    });
                self.finish_stream(id, r);
                return;
            }
            Request::ChangeRender {
                repo_id,
                path,
                change,
                opts,
                first_chunk,
            } => {
                let r = self
                    .daemon
                    .read(move |core| core.render_change(repo_id, &path, change, opts))
                    .await
                    .map(|(header, rendered)| {
                        stream_render(&self.outbox, id, header, &rendered, first_chunk);
                    });
                self.finish_stream(id, r);
                return;
            }
            Request::BlobRender {
                repo_id,
                path,
                blob_oid,
                first_chunk,
            } => {
                let r = self
                    .daemon
                    .read(move |core| core.blob_render(repo_id, &path, blob_oid))
                    .await
                    .map(|(header, rendered)| {
                        stream_render(&self.outbox, id, header, &rendered, first_chunk);
                    });
                self.finish_stream(id, r);
                return;
            }
            other => dispatch::single(&self.daemon, &self.negotiated, other).await,
        };
        debug_assert_eq!(shape, ResponseShape::Single);
        match result {
            Ok(response) => self.outbox.send(ServerMsg::Response { id, response }),
            Err(error) => self.outbox.send(ServerMsg::Error {
                id,
                error: error.into(),
            }),
        }
    }

    fn finish_stream(&self, id: RequestId, r: Result<(), DaemonError>) {
        if let Err(error) = r {
            self.outbox.send(ServerMsg::Error {
                id,
                error: error.into(),
            });
        }
        self.outbox.send(ServerMsg::StreamEnd { id });
    }

    /// Register a scope, replay the gap for `Since::After`, and report the
    /// position live events continue from.
    async fn subscribe(
        &self,
        scope: SubscribeScope,
        since: Since,
    ) -> Result<Response, DaemonError> {
        // Hold the lock across the replay so the live tail cannot interleave
        // an event the replay is about to send.
        let mut subs = self.subs.lock().await;
        let after = match since {
            Since::Now => None,
            Since::After { seq } => Some(seq),
        };
        // Include pending events for existing scopes too. Their live tail may
        // be waiting for this lock; replaying a newer event first would break
        // delivery order, and replaying overlap twice would duplicate events.
        // Re-subscribing replaces this scope's previous cursor; only other
        // active scopes may contribute pending events outside the requested gap.
        let active_after = subs
            .scopes
            .iter()
            .filter(|(active, _)| **active != scope)
            .map(|(_, delivered)| *delivered)
            .min();
        let replay_after = match (after, active_after) {
            (Some(requested), Some(active)) => Some(requested.min(active)),
            (requested, active) => requested.or(active),
        };
        let events = self
            .daemon
            .read(move |core| core.events_after(replay_after))
            .await?;
        for e in &events {
            // The caller's cursor acknowledges receipt. A previous subscription
            // may have queued these events without the caller consuming them.
            let requested = after.is_some_and(|seq| e.seq > seq) && self.daemon.matches(&scope, e);
            let pending = subs.scopes.iter().any(|(active, delivered)| {
                *active != scope && e.seq > *delivered && self.daemon.matches(active, e)
            });
            if requested || pending {
                self.outbox.send(ServerMsg::Event { event: e.clone() });
            }
        }
        let head = events.last().map(|e| e.seq).or(replay_after);
        let head = match head {
            Some(h) => h,
            None => self
                .daemon
                .read(nits_review_core::Core::last_seq)
                .await?
                .unwrap_or(Seq::new(0)),
        };
        // All active scopes have caught up to this read's head. Live copies
        // already in the broadcast queue must not repeat the replay above.
        for delivered in subs.scopes.values_mut() {
            *delivered = (*delivered).max(head);
        }
        let requested_head = after.map_or(head, |seq| head.max(seq));
        subs.scopes.insert(scope, requested_head);
        Ok(Response::Subscribed {
            seq: requested_head,
        })
    }
}

/// Header, then chunks starting at `first`, wrapping round to the start.
pub fn stream_render(
    out: &Outbox,
    id: RequestId,
    header: nits_protocol::FileRenderHeader,
    rendered: &nits_review_core::render::Rendered,
    first: ChunkIndex,
) {
    let repo_id = header.repo_id;
    let path = header.path.clone();
    out.send(ServerMsg::StreamItem {
        id,
        item: StreamItem::Header { header },
    });
    let n = rendered.chunk_count();
    let start = first.get().min(n.saturating_sub(1));
    for i in (start..n).chain(0..start) {
        if let Some(chunk) = rendered.chunk(ChunkIndex::new(i)) {
            send_chunk(out, id, repo_id, path.clone(), chunk);
        }
    }
}

pub fn send_chunk(
    out: &Outbox,
    id: RequestId,
    repo_id: nits_protocol::RepoId,
    path: nits_protocol::RepoPath,
    chunk: RenderChunk,
) {
    out.send(ServerMsg::StreamItem {
        id,
        item: StreamItem::Chunk {
            repo_id,
            path,
            chunk,
        },
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use nits_protocol::{Author, BuildInfo, ClientId, ClientSeq, WorkspaceId};
    use nits_review_core::DataDir;

    fn connection() -> (
        tempfile::TempDir,
        Connection,
        mpsc::UnboundedReceiver<ServerMsg>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let build = BuildInfo {
            name: "test".into(),
            version: "0".into(),
        };
        let daemon = Daemon::open(&DataDir::new(dir.path()), build.clone()).unwrap();
        let (tx, outgoing) = mpsc::unbounded_channel();
        let conn = Connection {
            daemon,
            negotiated: Negotiated {
                protocol: ProtocolVersion::CURRENT,
                client_id: ClientId::from_parts(1, 1),
                client: build,
                author: Author::Human {
                    name: "ada".into(),
                    machine: "box".into(),
                },
            },
            outbox: Outbox { tx },
            subs: Mutex::default(),
        };
        (dir, conn, outgoing)
    }

    struct Incoming(mpsc::UnboundedReceiver<Vec<u8>>);

    impl FrameRead for Incoming {
        async fn recv(&mut self) -> Result<Option<Vec<u8>>, CodecError> {
            Ok(self.0.recv().await)
        }
    }

    #[tokio::test]
    async fn dropping_input_loop_drops_a_subscription_waiting_for_its_lock() {
        let (_dir, conn, mut outgoing) = connection();
        let conn = Arc::new(conn);
        let locked = conn.subs.lock().await;
        let (input, frames) = mpsc::unbounded_channel();
        input
            .send(
                crate::codec::encode(&Envelope::current(ClientMsg::Request {
                    id: RequestId::new(1),
                    request: Request::Subscribe {
                        scope: SubscribeScope::All,
                        since: Since::Now,
                    },
                }))
                .unwrap(),
            )
            .unwrap();
        let mut read = Incoming(frames);
        let mut serving = Box::pin(read_loop(&conn, &mut read));
        // Polling to Pending drives the request up to the held subscription
        // lock. The extra Arc proves this is an active request, not just a
        // frame still queued at the transport boundary.
        assert!(futures_util::poll!(serving.as_mut()).is_pending());
        assert_eq!(Arc::strong_count(&conn), 2);
        assert!(outgoing.try_recv().is_err());
        drop(serving);
        assert_eq!(Arc::strong_count(&conn), 1);
        drop(locked);
        assert!(conn.subs.lock().await.scopes.is_empty());
    }

    #[tokio::test]
    async fn request_completion_preserves_a_partially_read_next_frame() {
        use tokio::io::AsyncWriteExt;
        let (_dir, conn, mut outgoing) = connection();
        let conn = Arc::new(conn);
        let locked = conn.subs.lock().await;
        let (mut client, server) = tokio::io::duplex(65536);
        let (mut read, _write) = transport::byte_stream(server);
        crate::codec::write_msg(
            &mut client,
            &Envelope::current(ClientMsg::Request {
                id: RequestId::new(1),
                request: Request::Subscribe {
                    scope: SubscribeScope::All,
                    since: Since::Now,
                },
            }),
        )
        .await
        .unwrap();
        let next = crate::codec::encode(&Envelope::current(ClientMsg::Hello {
            client_id: conn.negotiated.client_id,
            protocol: ProtocolVersion::CURRENT,
            client: conn.negotiated.client.clone(),
            author: conn.negotiated.author.clone(),
        }))
        .unwrap();
        let length = u32::try_from(next.len()).unwrap().to_be_bytes();
        client.write_all(&length[..2]).await.unwrap();
        let mut serving = Box::pin(read_loop(&conn, &mut read));
        // This poll consumes the first two prefix bytes and starts Subscribe,
        // which is blocked on the lock. Complete it before the remaining bytes.
        assert!(futures_util::poll!(serving.as_mut()).is_pending());
        assert_eq!(Arc::strong_count(&conn), 2);
        drop(locked);
        let subscribed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! {
                result = &mut serving => panic!("input loop ended: {result:?}"),
                message = outgoing.recv() => message.unwrap(),
            }
        })
        .await
        .unwrap();
        assert!(matches!(
            subscribed,
            ServerMsg::Response {
                response: Response::Subscribed { .. },
                ..
            }
        ));
        client.write_all(&length[2..]).await.unwrap();
        client.write_all(&next).await.unwrap();
        let reply = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! {
                result = &mut serving => panic!("input loop lost its frame: {result:?}"),
                message = outgoing.recv() => message.unwrap(),
            }
        })
        .await
        .unwrap();
        assert!(matches!(
            reply,
            ServerMsg::Error {
                error: RpcError::Invalid { .. },
                ..
            }
        ));
    }

    #[tokio::test]
    async fn cancelled_request_completion_does_not_forget_a_reused_id() {
        let (_dir, conn, mut outgoing) = connection();
        let conn = Arc::new(conn);
        let locked = conn.subs.lock().await;
        let (input, frames) = mpsc::unbounded_channel();
        let subscribe = ClientMsg::Request {
            id: RequestId::new(1),
            request: Request::Subscribe {
                scope: SubscribeScope::All,
                since: Since::Now,
            },
        };
        input
            .send(crate::codec::encode(&Envelope::current(subscribe.clone())).unwrap())
            .unwrap();
        let mut read = Incoming(frames);
        let mut serving = Box::pin(read_loop(&conn, &mut read));
        assert!(futures_util::poll!(serving.as_mut()).is_pending());
        for message in [
            ClientMsg::Cancel {
                id: RequestId::new(1),
            },
            subscribe,
        ] {
            input
                .send(crate::codec::encode(&Envelope::current(message)).unwrap())
                .unwrap();
        }
        assert!(futures_util::poll!(serving.as_mut()).is_pending());
        assert_eq!(
            outgoing.try_recv().unwrap(),
            ServerMsg::Error {
                id: RequestId::new(1),
                error: RpcError::Cancelled
            }
        );
        assert_eq!(
            outgoing.try_recv().unwrap(),
            ServerMsg::StreamEnd {
                id: RequestId::new(1)
            }
        );
        // Cancel the replacement too: finishing the old future must not remove
        // its cancellation token from the request-ID map.
        input
            .send(
                crate::codec::encode(&Envelope::current(ClientMsg::Cancel {
                    id: RequestId::new(1),
                }))
                .unwrap(),
            )
            .unwrap();
        assert!(futures_util::poll!(serving.as_mut()).is_pending());
        assert_eq!(
            outgoing.try_recv().unwrap(),
            ServerMsg::Error {
                id: RequestId::new(1),
                error: RpcError::Cancelled
            }
        );
        assert_eq!(
            outgoing.try_recv().unwrap(),
            ServerMsg::StreamEnd {
                id: RequestId::new(1)
            }
        );
        assert_eq!(Arc::strong_count(&conn), 1);
        drop(locked);
    }

    #[tokio::test]
    async fn subscribing_catches_up_existing_scopes_before_the_live_tail_resumes() {
        for since in [Since::Now, Since::After { seq: Seq::new(0) }] {
            let (_dir, conn, mut outgoing) = connection();
            conn.subscribe(SubscribeScope::All, Since::Now)
                .await
                .unwrap();
            let ctx = Daemon::ctx(
                conn.negotiated.author.clone(),
                conn.negotiated.client_id,
                ClientSeq::new(1),
            );
            let (_, events) = conn
                .daemon
                .write(move |core| {
                    core.create_workspace(&ctx, WorkspaceId::from_parts(1, 1), "first".into())?;
                    core.create_workspace(&ctx, WorkspaceId::from_parts(1, 2), "second".into())
                })
                .await
                .unwrap();

            // Both events are committed but their live delivery is still waiting
            // for the subscription lock. Only the second matches the new scope.
            let scope = SubscribeScope::Workspace {
                workspace_id: WorkspaceId::from_parts(1, 2),
            };
            conn.subscribe(scope.clone(), since).await.unwrap();
            for event in &events {
                assert_eq!(
                    outgoing.try_recv().unwrap(),
                    ServerMsg::Event {
                        event: event.clone()
                    }
                );
            }
            for event in &events {
                conn.deliver_live(&Arc::new(event.clone())).await;
            }
            assert!(
                outgoing.try_recv().is_err(),
                "no replay/live overlap duplicates"
            );

            // An explicit rewind still replays even while both scopes stay active.
            conn.subscribe(scope, Since::After { seq: Seq::new(0) })
                .await
                .unwrap();
            assert_eq!(
                outgoing.try_recv().unwrap(),
                ServerMsg::Event {
                    event: events[1].clone()
                }
            );
            assert!(outgoing.try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn replacing_a_scope_honors_the_new_cursor_instead_of_its_pending_live_events() {
        for since in [Since::Now, Since::After { seq: Seq::FIRST }] {
            let (_dir, conn, mut outgoing) = connection();
            conn.subscribe(SubscribeScope::All, Since::Now)
                .await
                .unwrap();
            let ctx = Daemon::ctx(
                conn.negotiated.author.clone(),
                conn.negotiated.client_id,
                ClientSeq::new(1),
            );
            let (_, events) = conn
                .daemon
                .write(move |core| {
                    core.create_workspace(&ctx, WorkspaceId::from_parts(1, 1), "first".into())
                })
                .await
                .unwrap();
            assert_eq!(events[0].seq, Seq::FIRST);
            conn.subscribe(SubscribeScope::All, since).await.unwrap();
            conn.deliver_live(&Arc::new(events[0].clone())).await;
            assert!(outgoing.try_recv().is_err());
        }
    }
}
