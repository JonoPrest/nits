//! Real SSH proxy processes: their stdin stays open after daemon disconnection.
//! Every wait is bounded and children are killed on drop, including failures.

use std::io::{Seek as _, Write as _};
use std::os::fd::OwnedFd;
use std::process::Stdio;
use std::time::Duration;

use rustix::fs::{OFlags, fcntl_getfl};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{UnixListener, UnixStream};

const DEADLINE: Duration = Duration::from_secs(3);
const REQUEST: &[u8] = b"one request, never replayed\n";

struct Proxy {
    dir: tempfile::TempDir,
    listener: UnixListener,
    command: tokio::process::Command,
}

impl Proxy {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_nits"));
        command
            .env("NITS_CONFIG", dir.path().join("no-config.toml"))
            .env_remove("NITS_CONTEXT")
            .env_remove("NITS_SOCKET")
            .env_remove("NITS_WS")
            .env_remove("NITS_WS_URL")
            .args(["daemon", "stdio", "--start-policy", "require-running"])
            .arg("--socket")
            .arg(socket)
            .arg("--data-dir")
            .arg(dir.path())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        Self {
            dir,
            listener,
            command,
        }
    }

    async fn connection(&self) -> UnixStream {
        // serve::stdio first probes liveness and then opens the byte proxy.
        let (mut probe, _) = tokio::time::timeout(DEADLINE, self.listener.accept())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            tokio::time::timeout(DEADLINE, probe.read(&mut [0; 1]))
                .await
                .unwrap()
                .unwrap(),
            0
        );
        tokio::time::timeout(DEADLINE, self.listener.accept())
            .await
            .unwrap()
            .unwrap()
            .0
    }
}

async fn output(child: &mut tokio::process::Child) -> std::process::Output {
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    tokio::time::timeout(DEADLINE, async {
        let (status, stdout, stderr) = tokio::join!(
            child.wait(),
            async {
                let mut bytes = Vec::new();
                stdout.read_to_end(&mut bytes).await.unwrap();
                bytes
            },
            async {
                let mut bytes = Vec::new();
                stderr.read_to_end(&mut bytes).await.unwrap();
                bytes
            },
        );
        std::process::Output {
            status: status.unwrap(),
            stdout,
            stderr,
        }
    })
    .await
    .expect("proxy must exit without its caller closing stdin")
}

async fn request(upstream: &mut UnixStream) {
    let mut bytes = vec![0; REQUEST.len()];
    tokio::time::timeout(DEADLINE, upstream.read_exact(&mut bytes))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bytes, REQUEST);
}

/// Confirm bytes are unread before dropping the peer, deterministically
/// generating ECONNRESET instead of depending on a sleep to race a write.
async fn unread(upstream: &UnixStream) {
    tokio::time::timeout(DEADLINE, async {
        loop {
            upstream.readable().await.unwrap();
            match upstream.try_io(tokio::io::Interest::READABLE, || {
                rustix::net::recv(upstream, &mut [0; 1], rustix::net::RecvFlags::PEEK)
                    .map_err(std::io::Error::from)
            }) {
                Ok((_, count)) => {
                    assert!(count > 0);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("peeking upstream: {error}"),
            }
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn upstream_eof_drains_stdout_and_exits_with_stdin_open() {
    let mut proxy = Proxy::new();
    let (input, mut caller) = std::io::pipe().unwrap();
    let flags = fcntl_getfl(&input).unwrap();
    proxy.command.stdin(input.try_clone().unwrap());
    let mut child = proxy.command.spawn().unwrap();
    let mut upstream = proxy.connection().await;
    caller.write_all(REQUEST).unwrap();
    request(&mut upstream).await;
    let response: Vec<u8> = (0..3 * 1024 * 1024)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    let (result, ()) = tokio::join!(output(&mut child), async {
        upstream.write_all(&response).await.unwrap();
        // Read half remains open: the proxy must act on upstream write EOF.
        upstream.shutdown().await.unwrap();
    });
    assert!(result.status.success(), "{result:?}");
    assert_eq!(result.stdout, response);
    assert_eq!(fcntl_getfl(&input).unwrap(), flags);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), proxy.listener.accept())
            .await
            .is_err()
    );
    drop(caller);
}

#[tokio::test]
async fn upstream_reset_reports_error_and_exits_with_stdin_open() {
    let mut proxy = Proxy::new();
    let (input, mut caller) = std::io::pipe().unwrap();
    let flags = fcntl_getfl(&input).unwrap();
    proxy.command.stdin(input.try_clone().unwrap());
    let mut child = proxy.command.spawn().unwrap();
    let mut upstream = proxy.connection().await;
    caller.write_all(REQUEST).unwrap();
    request(&mut upstream).await;
    caller.write_all(b"still unread").unwrap();
    unread(&upstream).await;
    drop(upstream);
    let result = output(&mut child).await;
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("Connection reset"),
        "{result:?}"
    );
    assert_eq!(fcntl_getfl(&input).unwrap(), flags);
    drop(caller);
}

#[tokio::test]
async fn stdin_eof_half_closes_requests_and_drains_late_responses() {
    let mut proxy = Proxy::new();
    proxy.command.stdin(Stdio::piped());
    let mut child = proxy.command.spawn().unwrap();
    let mut upstream = proxy.connection().await;
    let mut input = child.stdin.take().unwrap();
    input.write_all(REQUEST).await.unwrap();
    drop(input);
    let mut received = Vec::new();
    tokio::time::timeout(DEADLINE, upstream.read_to_end(&mut received))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(received, REQUEST);
    assert!(child.try_wait().unwrap().is_none());
    let response = vec![b'r'; 1024 * 1024];
    let (result, ()) = tokio::join!(output(&mut child), async {
        upstream.write_all(&response).await.unwrap();
        upstream.shutdown().await.unwrap();
    });
    assert!(result.status.success(), "{result:?}");
    assert_eq!(result.stdout, response);
}

#[tokio::test]
async fn regular_file_and_null_stdin_keep_eof_semantics_and_flags() {
    for bytes in [REQUEST, b"".as_slice()] {
        let mut proxy = Proxy::new();
        let input = if bytes.is_empty() {
            std::fs::File::open("/dev/null").unwrap()
        } else {
            let path = proxy.dir.path().join("requests");
            std::fs::write(&path, bytes).unwrap();
            std::fs::File::open(path).unwrap()
        };
        let flags = fcntl_getfl(&input).unwrap();
        proxy.command.stdin(input.try_clone().unwrap());
        let mut child = proxy.command.spawn().unwrap();
        let mut upstream = proxy.connection().await;
        let mut received = Vec::new();
        tokio::time::timeout(DEADLINE, upstream.read_to_end(&mut received))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(received, bytes);
        upstream.write_all(b"final response").await.unwrap();
        upstream.shutdown().await.unwrap();
        let result = output(&mut child).await;
        assert!(result.status.success(), "{result:?}");
        assert_eq!(result.stdout, b"final response");
        assert_eq!(fcntl_getfl(&input).unwrap(), flags);
    }
}

async fn open_descriptor(input: &OwnedFd, caller: &mut impl std::io::Write) {
    let mut proxy = Proxy::new();
    let flags = fcntl_getfl(input).unwrap();
    proxy.command.stdin(input.try_clone().unwrap());
    let mut child = proxy.command.spawn().unwrap();
    let mut upstream = proxy.connection().await;
    caller.write_all(REQUEST).unwrap();
    request(&mut upstream).await;
    upstream.shutdown().await.unwrap();
    let result = output(&mut child).await;
    assert!(result.status.success(), "{result:?}");
    assert_eq!(fcntl_getfl(input).unwrap(), flags);
}

#[tokio::test]
async fn socket_and_terminal_stdin_are_cancellable_and_restore_flags() {
    let (input, mut caller) = std::os::unix::net::UnixStream::pair().unwrap();
    input.set_nonblocking(true).unwrap();
    open_descriptor(&input.into(), &mut caller).await;
    let master =
        rustix::pty::openpt(rustix::pty::OpenptFlags::RDWR | rustix::pty::OpenptFlags::NOCTTY)
            .unwrap();
    rustix::pty::grantpt(&master).unwrap();
    rustix::pty::unlockpt(&master).unwrap();
    let name = rustix::pty::ptsname(&master, Vec::new()).unwrap();
    let slave = rustix::fs::open(
        name,
        OFlags::RDONLY | OFlags::NOCTTY,
        rustix::fs::Mode::empty(),
    )
    .unwrap();
    let mut caller = std::fs::File::from(master);
    open_descriptor(&slave, &mut caller).await;
}

#[tokio::test]
async fn upstream_reset_cancels_a_worker_blocked_on_input_backpressure() {
    let mut proxy = Proxy::new();
    let path = proxy.dir.path().join("large-requests");
    std::fs::write(&path, vec![b'q'; 8 * 1024 * 1024]).unwrap();
    let mut input = std::fs::File::open(path).unwrap();
    let flags = fcntl_getfl(&input).unwrap();
    proxy.command.stdin(input.try_clone().unwrap());
    let mut child = proxy.command.spawn().unwrap();
    let upstream = proxy.connection().await;
    unread(&upstream).await;
    // Once the socket fills, the bounded input channel fills too. Observing
    // its shared file offset stop proves the worker has reached backpressure.
    tokio::time::timeout(DEADLINE, async {
        loop {
            let before = input.stream_position().unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            let after = input.stream_position().unwrap();
            if before > 0 && before == after {
                assert!(after < 8 * 1024 * 1024);
                break;
            }
        }
    })
    .await
    .unwrap();
    drop(upstream);
    let result = output(&mut child).await;
    assert!(!result.status.success());
    assert_eq!(fcntl_getfl(&input).unwrap(), flags);
}

impl Proxy {
    fn ssh_command(&self, operation: &str) -> tokio::process::Command {
        let ssh = self.dir.path().join("ssh");
        std::fs::write(&ssh, "#!/bin/sh\nshift\nexec \"$@\"\n").unwrap();
        std::fs::set_permissions(&ssh, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let mut config = nits_config::Config::default();
        config.contexts.insert(
            "remote".parse().unwrap(),
            nits_config::Context::Ssh {
                host: "test-host".into(),
                bin: nits_config::RemoteBin::Nits(env!("CARGO_BIN_EXE_nits").into()),
                args: vec![
                    "--socket".into(),
                    self.dir
                        .path()
                        .join("daemon.sock")
                        .to_string_lossy()
                        .into_owned(),
                    "--data-dir".into(),
                    self.dir.path().to_string_lossy().into_owned(),
                ],
                ssh: Some(ssh.to_string_lossy().into_owned()),
            },
        );
        let path = self.dir.path().join("config.toml");
        config.save(&path).unwrap();
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_nits"));
        command
            .env("NITS_CONFIG", path)
            .env_remove("NITS_SOCKET")
            .env_remove("NITS_WS")
            .env_remove("NITS_WS_URL")
            .args(["--context", "remote", "daemon", operation])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command
    }
}

async fn acknowledge_shutdown(upstream: UnixStream) {
    use nits_protocol::{
        BuildInfo, ClientMsg, Envelope, ProtocolVersion, Request, Response, SchemaVersion,
        ServerMsg,
    };
    use nitsd::transport::{byte_stream, recv_msg, send_msg};
    let (mut read, mut write) = byte_stream(upstream);
    let hello = recv_msg::<_, ClientMsg>(&mut read).await.unwrap().unwrap();
    assert!(matches!(hello.msg, ClientMsg::Hello { .. }));
    send_msg(
        &mut write,
        &Envelope {
            v: ProtocolVersion::CURRENT,
            msg: ServerMsg::Welcome {
                protocol: ProtocolVersion::CURRENT,
                daemon: BuildInfo {
                    name: "test-daemon".into(),
                    version: "test".into(),
                },
                schema: SchemaVersion::CURRENT,
                upgrade: None,
            },
        },
    )
    .await
    .unwrap();
    let shutdown = recv_msg::<_, ClientMsg>(&mut read).await.unwrap().unwrap();
    let id = match shutdown.msg {
        ClientMsg::Request {
            id,
            request: Request::Shutdown,
        } => id,
        other => panic!("expected shutdown, got {other:?}"),
    };
    send_msg(
        &mut write,
        &Envelope {
            v: ProtocolVersion::CURRENT,
            msg: ServerMsg::Response {
                id,
                response: Response::ShuttingDown,
            },
        },
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn ssh_stop_and_status_finish_when_the_probe_connection_resets() {
    for operation in ["stop", "status"] {
        let proxy = Proxy::new();
        let mut child = proxy.ssh_command(operation).spawn().unwrap();
        if operation == "stop" {
            let upstream = proxy.connection().await;
            tokio::time::timeout(DEADLINE, acknowledge_shutdown(upstream))
                .await
                .unwrap();
        }
        // During stop's follow-up handshake (or status's initial probe),
        // leave its Hello unread and close: the real stdio subprocess must
        // close stdout promptly despite the SSH caller retaining stdin.
        let upstream = proxy.connection().await;
        unread(&upstream).await;
        drop(upstream);
        let result = output(&mut child).await;
        assert!(result.status.success(), "{operation}: {result:?}");
        let stdout = String::from_utf8(result.stdout).unwrap();
        if operation == "stop" {
            assert_eq!(stdout.trim(), "stopping");
        } else {
            assert!(stdout.trim().ends_with("stopped"), "{stdout}");
        }
    }
}
