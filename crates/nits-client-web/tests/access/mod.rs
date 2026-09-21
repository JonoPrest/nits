//! Wire-level tests intentionally retain malformed/duplicate headers.

use super::*;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

async fn request(addr: SocketAddr, path: &str, headers: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    stream
        .write_all(format!("GET {path} HTTP/1.1\r\n{headers}\r\n").as_bytes())
        .await
        .unwrap();
    let mut reply = vec![0; 4096];
    let count = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut reply))
        .await
        .unwrap()
        .unwrap();
    let end = reply[..count]
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap()
        + 4;
    String::from_utf8(reply[..end].to_vec()).unwrap()
}

const UPGRADE: &str = "Connection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n";

#[tokio::test]
async fn hostile_requests_are_rejected_before_starting_a_host() {
    let config = nits_client_web::web_config(
        DaemonEndpoint::WebSocket {
            url: "ws://127.0.0.1:1".into(),
        },
        client_info(),
        author(),
        IdSeed(500),
        KvConfig::Memory,
    );
    let bridge = nits_client_web::serve((Ipv4Addr::LOCALHOST, 0).into(), config)
        .await
        .unwrap();
    let addr = bridge.addr();
    let host = format!("Host: {addr}\r\n");
    let origin = format!("Origin: http://{addr}\r\n");
    for headers in [
        format!("{host}{UPGRADE}"), // No Origin is not an authentication bypass.
        format!("{host}Origin: null\r\n{UPGRADE}"),
        format!("{host}Origin: http://unrelated.example\r\n{UPGRADE}"),
        format!("{host}Origin: http://127.0.0.1:1\r\n{UPGRADE}"),
        format!("{host}Origin: https://{addr}\r\n{UPGRADE}"),
        format!(
            "{host}Origin: http://localhost:{}\r\n{UPGRADE}",
            addr.port()
        ),
        format!("{host}{origin}{origin}{UPGRADE}"),
        format!("{host}{origin}Origin: http://unrelated.example\r\n{UPGRADE}"),
        format!("{host}{host}{origin}{UPGRADE}"),
        format!("{origin}{UPGRADE}"),
        format!("Host: rebound.example\r\nOrigin: http://rebound.example\r\n{UPGRADE}"),
        format!("Host: {addr}.unrelated.example\r\n{origin}{UPGRADE}"),
        format!("Host: {addr}, unrelated.example\r\n{origin}{UPGRADE}"),
        format!("{host}Origin: http://{addr}/\r\n{UPGRADE}"),
        format!("{host}Origin: http://{addr} http://unrelated.example\r\n{UPGRADE}"),
        format!("Host: unrelated.example\r\n{origin}X-Forwarded-Host: {addr}\r\n{UPGRADE}"),
        format!(
            "{host}Origin: http://unrelated.example\r\nX-Forwarded-Origin: http://{addr}\r\n{UPGRADE}"
        ),
    ] {
        let reply = request(addr, "/ws", &headers).await;
        assert!(reply.starts_with("HTTP/1.1 403"), "{headers:?}: {reply}");
        assert_eq!(bridge.active_sessions(), 0);
    }
    // Host validation applies to the app shell too, preventing rebinding from
    // loading the UI under an attacker-controlled hostname.
    assert!(
        request(addr, "/", "Host: rebound.example\r\n")
            .await
            .starts_with("HTTP/1.1 403")
    );
    for path in ["/ws-extra", "/ws/", "/ws?token=ignored", "/%77s"] {
        assert!(
            request(addr, path, &format!("{host}{origin}{UPGRADE}"))
                .await
                .starts_with("HTTP/1.1 404"),
            "{path}"
        );
        assert_eq!(bridge.active_sessions(), 0);
    }
    for path in ["/ws#ignored", "http://unrelated.example/ws"] {
        assert!(
            request(addr, path, &format!("{host}{origin}{UPGRADE}"))
                .await
                .starts_with("HTTP/1.1 400")
        );
    }
    let invalid = UPGRADE.replace("Sec-WebSocket-Version: 13", "Sec-WebSocket-Version: 12");
    assert!(
        request(addr, "/ws", &format!("{host}{origin}{invalid}"))
            .await
            .starts_with("HTTP/1.1 400")
    );
    assert_eq!(bridge.active_sessions(), 0);
}

#[tokio::test]
async fn local_alias_and_explicit_proxy_origins_work_with_preserved_or_rewritten_host() {
    let h = harness().await;
    let mut config = nits_client_web::web_config(
        h.endpoint.clone(),
        client_info(),
        author(),
        IdSeed(600),
        KvConfig::Memory,
    );
    config.allowed_origins = vec![
        "http://localhost:5173".parse().unwrap(),
        "https://review.example".parse().unwrap(),
    ];
    let bridge = nits_client_web::serve((Ipv4Addr::LOCALHOST, 0).into(), config)
        .await
        .unwrap();
    let addr = bridge.addr();
    for (host, origin) in [
        (addr.to_string(), format!("http://{addr}")),
        (
            format!("localhost:{}", addr.port()),
            format!("http://localhost:{}", addr.port()),
        ),
        ("localhost:5173".into(), "http://localhost:5173".into()),
        (addr.to_string(), "http://localhost:5173".into()),
        ("review.example".into(), "https://review.example".into()),
        ("review.example:443".into(), "https://review.example".into()),
    ] {
        let reply = request(
            addr,
            "/ws",
            &format!("Host: {host}\r\nOrigin: {origin}\r\n{UPGRADE}"),
        )
        .await;
        assert!(
            reply.starts_with("HTTP/1.1 101"),
            "{host} {origin}: {reply}"
        );
    }
    wait_for_sessions(&bridge, 0).await;
    assert!(
        request(
            addr,
            "/ws",
            &format!("Host: localhost:5173\r\nOrigin: http://unrelated.example\r\n{UPGRADE}")
        )
        .await
        .starts_with("HTTP/1.1 403")
    );
    assert!(
        request(
            addr,
            "/ws",
            &format!("Host: unrelated.example\r\nOrigin: http://localhost:5173\r\n{UPGRADE}")
        )
        .await
        .starts_with("HTTP/1.1 403")
    );
    assert!(
        request(
            addr,
            "/ws",
            &format!("Host: localhost:5173:80\r\nOrigin: http://localhost:5173\r\n{UPGRADE}")
        )
        .await
        .starts_with("HTTP/1.1 403")
    );
    assert_eq!(bridge.active_sessions(), 0);
}
