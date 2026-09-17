//! Deterministic transport faults: no socket timing or daemon scheduling needed.

use std::future::Future;
use std::time::Duration;

use super::*;
use tokio_util::sync::CancellationToken;

struct ReadFrames(mpsc::UnboundedReceiver<Result<Vec<u8>, CodecError>>);

impl FrameRead for ReadFrames {
    async fn recv(&mut self) -> Result<Option<Vec<u8>>, CodecError> {
        self.0.recv().await.transpose()
    }
}

struct WriteFrames {
    frames: mpsc::UnboundedSender<Vec<u8>>,
    fail: CancellationToken,
}

impl FrameWrite for WriteFrames {
    async fn send(&mut self, frame: &[u8]) -> Result<(), CodecError> {
        if self.fail.is_cancelled() {
            return Err(std::io::Error::from(std::io::ErrorKind::BrokenPipe).into());
        }
        self.frames.send(frame.to_vec()).unwrap();
        Ok(())
    }

    async fn close(&mut self) -> Result<(), CodecError> {
        Ok(())
    }
}

struct Peer {
    incoming: mpsc::UnboundedSender<Result<Vec<u8>, CodecError>>,
    outgoing: mpsc::UnboundedReceiver<Vec<u8>>,
    fail_write: CancellationToken,
}

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(3), future)
        .await
        .expect("connection operation hung")
}

async fn connect() -> (Arc<Client>, Peer) {
    let (incoming, rx) = mpsc::unbounded_channel();
    let (tx, mut outgoing) = mpsc::unbounded_channel();
    let fail_write = CancellationToken::new();
    incoming
        .send(Ok(crate::codec::encode(&Envelope::current(
            ServerMsg::Welcome {
                protocol: ProtocolVersion::CURRENT,
                daemon: BuildInfo {
                    name: "test".into(),
                    version: "0".into(),
                },
                schema: SchemaVersion::CURRENT,
                upgrade: None,
            },
        ))
        .unwrap()))
        .unwrap();
    let client = bounded(Client::handshake_framed(
        ReadFrames(rx),
        WriteFrames {
            frames: tx,
            fail: fail_write.clone(),
        },
        Identity {
            client_id: ClientId::from_parts(1, 1),
            client: BuildInfo {
                name: "test".into(),
                version: "0".into(),
            },
            author: Author::Human {
                name: "ada".into(),
                machine: "box".into(),
            },
        },
        ProtocolVersion::CURRENT,
    ))
    .await
    .unwrap();
    bounded(outgoing.recv()).await.unwrap(); // Hello.
    (
        Arc::new(client),
        Peer {
            incoming,
            outgoing,
            fail_write,
        },
    )
}

fn stream_request() -> Request {
    Request::OpenReview {
        review_id: nits_protocol::ReviewId::from_parts(1, 1),
        opts: nits_protocol::RenderOpts::default(),
    }
}

async fn assert_closed(client: &Client) {
    assert!(bounded(client.next_unsolicited()).await.is_none());
    assert!(client.is_closed());
    assert!(matches!(
        bounded(client.request(Request::ListWorkspaces)).await,
        Err(ClientError::Closed)
    ));
    assert!(matches!(
        bounded(client.stream(stream_request())).await,
        Err(ClientError::Closed)
    ));
    assert!(matches!(
        *client.state.lock().await,
        ConnectionState::Closed
    ));
}

#[tokio::test]
async fn eof_before_a_request_rejects_later_registration() {
    let (client, peer) = connect().await;
    drop(peer.incoming);
    assert_closed(&client).await;
}

#[tokio::test]
async fn eof_and_decode_failure_complete_in_flight_calls_and_streams() {
    for frame in [None, Some(b"invalid json".to_vec())] {
        let (client, mut peer) = connect().await;
        let (_, mut stream) = client.stream(stream_request()).await.unwrap();
        bounded(peer.outgoing.recv()).await.unwrap();
        let request_client = Arc::clone(&client);
        let request =
            tokio::spawn(async move { request_client.request(Request::ListWorkspaces).await });
        bounded(peer.outgoing.recv()).await.unwrap();
        if let Some(frame) = frame {
            peer.incoming.send(Ok(frame)).unwrap();
        } else {
            drop(peer.incoming);
        }
        assert!(matches!(
            bounded(request).await.unwrap(),
            Err(ClientError::Closed)
        ));
        assert!(matches!(
            bounded(stream.recv()).await,
            Some(Err(ClientError::Closed))
        ));
        assert!(bounded(stream.recv()).await.is_none());
        assert_closed(&client).await;
        // Reader failure also releases the writer, even with Client alive.
        assert!(bounded(peer.outgoing.recv()).await.is_none());
    }
}

#[tokio::test]
async fn writer_failure_clears_pending_calls_without_waiting_for_read_eof() {
    let (client, mut peer) = connect().await;
    let (_, mut stream) = client.stream(stream_request()).await.unwrap();
    bounded(peer.outgoing.recv()).await.unwrap();
    let waiting_client = Arc::clone(&client);
    let waiting =
        tokio::spawn(async move { waiting_client.request(Request::ListWorkspaces).await });
    bounded(peer.outgoing.recv()).await.unwrap();
    peer.fail_write.cancel();
    assert!(matches!(
        bounded(client.request(Request::ListWorkspaces)).await,
        Err(ClientError::Closed)
    ));
    assert!(matches!(
        bounded(waiting).await.unwrap(),
        Err(ClientError::Closed)
    ));
    assert!(matches!(
        bounded(stream.recv()).await,
        Some(Err(ClientError::Closed))
    ));
    assert!(bounded(stream.recv()).await.is_none());
    assert_closed(&client).await;
    // The peer's read direction stayed open; writer failure must cancel it.
    bounded(peer.incoming.closed()).await;
}

#[tokio::test]
async fn registration_waiting_on_the_cleanup_lock_cannot_escape_closure() {
    let (client, peer) = connect().await;
    let guard = client.state.lock().await;
    drop(peer.incoming);
    // Wait until I/O has stopped and cleanup is waiting on the held lock.
    bounded(async {
        while !client.is_closed() {
            tokio::task::yield_now().await;
        }
    })
    .await;
    let request = client.request(Request::ListWorkspaces);
    tokio::pin!(request);
    tokio::select! {
        biased;
        result = &mut request => panic!("request escaped the held lock: {result:?}"),
        () = std::future::ready(()) => {}
    }
    drop(guard);
    assert!(matches!(bounded(request).await, Err(ClientError::Closed)));
    assert_closed(&client).await;
}

#[tokio::test]
async fn stream_end_is_success_even_if_transport_closes_afterwards() {
    let (client, mut peer) = connect().await;
    let (id, mut stream) = client.stream(stream_request()).await.unwrap();
    bounded(peer.outgoing.recv()).await.unwrap();
    peer.incoming
        .send(Ok(crate::codec::encode(&Envelope::current(
            ServerMsg::StreamEnd { id },
        ))
        .unwrap()))
        .unwrap();
    assert!(bounded(stream.recv()).await.is_none());
    drop(peer.incoming);
    assert_closed(&client).await;
}
