//! Cancellable stdin for the SSH byte proxy. Tokio's stdin uses a blocking
//! read that runtime shutdown cannot cancel. A scoped worker polls stdin and
//! a cancellation socket instead; closing either its bounded channel or that
//! socket wakes every wait, and the owner always joins it before returning.

use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream as CancelSocket;
use std::thread::JoinHandle;

use rustix::event::{PollFd, PollFlags, poll};
use rustix::fs::{OFlags, fcntl_getfl, fcntl_setfl};
use tokio::io::{AsyncWrite, AsyncWriteExt as _};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

pub(super) async fn proxy(upstream: UnixStream) -> io::Result<()> {
    let mut input = InputPump::new(io::stdin().as_fd().try_clone_to_owned()?)?;
    let (mut read, mut write) = upstream.into_split();
    let mut stdout = tokio::io::stdout();
    let result = {
        let send = input.forward(&mut write);
        let receive = async { tokio::io::copy(&mut read, &mut stdout).await.map(|_| ()) };
        tokio::pin!(receive);
        tokio::select! {
            result = send => match result {
                Ok(()) => receive.await,
                Err(error) => Err(error),
            },
            result = &mut receive => result,
        }
    };
    let stopped = input.stop();
    // Flush bytes already copied, including when either forwarding half
    // failed. The caller must drain stdout to receive the complete response.
    let flushed = stdout.flush().await;
    result.and(stopped).and(flushed)
}

#[derive(Debug)]
struct InputPump {
    chunks: mpsc::Receiver<Vec<u8>>,
    cancel: Option<CancelSocket>,
    worker: Option<JoinHandle<io::Result<()>>>,
}

impl InputPump {
    fn new(input: OwnedFd) -> io::Result<Self> {
        let (cancel, cancelled) = CancelSocket::pair()?;
        // At most one queued chunk plus one in each forwarding stage.
        let (sender, chunks) = mpsc::channel(1);
        let worker = std::thread::Builder::new()
            .name("nits-stdio-input".into())
            .spawn(move || {
                let input = NonblockingInput::new(input)?;
                let result = read_input(&input.fd, &cancelled, &sender);
                let restored = input.restore();
                result.and(restored)
            })?;
        Ok(Self {
            chunks,
            cancel: Some(cancel),
            worker: Some(worker),
        })
    }

    async fn forward(&mut self, writer: &mut (impl AsyncWrite + Unpin)) -> io::Result<()> {
        while let Some(chunk) = self.chunks.recv().await {
            writer.write_all(&chunk).await?;
        }
        self.stop()?;
        writer.shutdown().await
    }

    fn stop(&mut self) -> io::Result<()> {
        // Wake a worker blocked on a full channel BEFORE joining it, as
        // well as one polling for input. Neither wait depends on stdin EOF.
        self.chunks.close();
        self.cancel.take();
        match self.worker.take() {
            Some(worker) => worker
                .join()
                .map_err(|_| io::Error::other("stdio input worker panicked"))?,
            None => Ok(()),
        }
    }
}

impl Drop for InputPump {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            tracing::warn!(%error, "stopping stdio input");
        }
    }
}

/// Nonblocking reads close the poll/read race without changing stdin's
/// flags permanently. `poll` also supports regular files and /dev/null,
/// unlike Linux epoll, so redirected input needs no special blocking path.
#[derive(Debug)]
struct NonblockingInput {
    fd: OwnedFd,
    original: OFlags,
}

impl NonblockingInput {
    fn new(fd: OwnedFd) -> io::Result<Self> {
        let original = fcntl_getfl(&fd)?;
        fcntl_setfl(&fd, original | OFlags::NONBLOCK)?;
        Ok(Self { fd, original })
    }

    fn restore(&self) -> io::Result<()> {
        fcntl_setfl(&self.fd, self.original)?;
        Ok(())
    }
}

impl Drop for NonblockingInput {
    fn drop(&mut self) {
        if let Err(error) = self.restore() {
            tracing::warn!(%error, "restoring stdin flags");
        }
    }
}

fn read_input(
    input: &OwnedFd,
    cancelled: &CancelSocket,
    sender: &mpsc::Sender<Vec<u8>>,
) -> io::Result<()> {
    let mut buffer = [0; 8192];
    loop {
        let mut ready = [
            PollFd::new(input, PollFlags::IN),
            PollFd::new(cancelled, PollFlags::IN),
        ];
        match poll(&mut ready, None) {
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => return Err(error.into()),
        }
        if !ready[1].revents().is_empty() {
            return Ok(());
        }
        if ready[0].revents().contains(PollFlags::NVAL) {
            return Err(io::Error::other("stdin descriptor is invalid"));
        }
        match rustix::io::read(input, &mut buffer) {
            Ok(0) => return Ok(()),
            Ok(count) => {
                if sender.blocking_send(buffer[..count].to_vec()).is_err() {
                    return Ok(());
                }
            }
            Err(rustix::io::Errno::INTR | rustix::io::Errno::AGAIN) => {}
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn cancelling_a_forwarder_joins_its_reader_and_restores_flags() {
        let (read, write) = std::io::pipe().unwrap();
        let original = fcntl_getfl(&read).unwrap();
        let mut input = InputPump::new(read.as_fd().try_clone_to_owned().unwrap()).unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !fcntl_getfl(&read).unwrap().contains(OFlags::NONBLOCK) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let mut output = tokio::io::sink();
        assert!(
            tokio::time::timeout(Duration::from_millis(20), input.forward(&mut output))
                .await
                .is_err()
        );
        drop(input);
        assert_eq!(fcntl_getfl(&read).unwrap(), original);
        drop(write);
    }

    #[tokio::test]
    async fn input_errors_are_reported_and_restore_flags() {
        let directory = tempfile::tempdir().unwrap();
        let read = std::fs::File::open(directory.path()).unwrap();
        let original = fcntl_getfl(&read).unwrap();
        let mut input = InputPump::new(read.as_fd().try_clone_to_owned().unwrap()).unwrap();
        let mut output = tokio::io::sink();
        let error = tokio::time::timeout(Duration::from_secs(1), input.forward(&mut output))
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::IsADirectory);
        assert_eq!(fcntl_getfl(&read).unwrap(), original);
    }
}
