//! Bounded HTTP parsing shared by static assets and the WebSocket handshake.

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue, Request, Version};

pub(crate) type RequestHead = Request<()>;

const MAX_HEAD: usize = 16 * 1024;

fn invalid(error: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error)
}

/// Preserve duplicate headers for the access policy to reject, and retain any
/// bytes read after the head so the first WebSocket frame is not discarded.
pub(crate) async fn read_head(stream: &mut TcpStream) -> std::io::Result<(RequestHead, Vec<u8>)> {
    let mut buf = Vec::with_capacity(1024);
    let mut byte = [0u8; 1024];
    let end = loop {
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            if end + 4 > MAX_HEAD {
                return Err(invalid("request head too large"));
            }
            break end + 4;
        }
        if buf.len() >= MAX_HEAD {
            return Err(invalid("request head too large"));
        }
        let n = stream.read(&mut byte).await?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf.extend_from_slice(&byte[..n]);
    };
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut raw = httparse::Request::new(&mut headers);
    if !raw.parse(&buf[..end]).map_err(invalid)?.is_complete() {
        return Err(invalid("incomplete request head"));
    }
    let target = raw.path.ok_or_else(|| invalid("missing path"))?;
    // The URI parser discards fragments; reject them before parsing so a
    // lookalike raw target can never become the privileged /ws route.
    if !target.starts_with('/') || target.contains('#') {
        return Err(invalid(
            "expected an origin-form request target without a fragment",
        ));
    }
    let mut request = Request::builder()
        .method(raw.method.ok_or_else(|| invalid("missing method"))?)
        .uri(target)
        .version(match raw.version {
            Some(1) => Version::HTTP_11,
            Some(0) => Version::HTTP_10,
            _ => return Err(invalid("invalid HTTP version")),
        })
        .body(())
        .map_err(invalid)?;
    for header in raw.headers {
        request.headers_mut().append(
            HeaderName::from_bytes(header.name.as_bytes()).map_err(invalid)?,
            HeaderValue::from_bytes(header.value).map_err(invalid)?,
        );
    }
    Ok((request, buf[end..].to_vec()))
}

pub(crate) async fn reject(stream: &mut TcpStream, status: &str) -> std::io::Result<()> {
    stream
        .write_all(
            format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                .as_bytes(),
        )
        .await
}
