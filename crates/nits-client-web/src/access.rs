//! Browser origin boundary, including the Host check that prevents DNS rebinding.

use std::net::SocketAddr;
use std::str::FromStr;

use tokio_tungstenite::tungstenite::http::{HeaderMap, HeaderValue, header};

/// A canonical HTTP(S) origin, with no credentials, path, query, or fragment.
/// Browsers serialize Origin this way; opaque origins such as `null` are rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserOrigin(url::Url);

#[derive(Debug, thiserror::Error)]
#[error("expected an exact http:// or https:// origin without a path or trailing slash")]
pub struct InvalidBrowserOrigin;

impl FromStr for BrowserOrigin {
    type Err = InvalidBrowserOrigin;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let url = url::Url::parse(value).map_err(|_| InvalidBrowserOrigin)?;
        if !matches!(url.scheme(), "http" | "https")
            || url.host_str().is_some_and(|host| host.contains('*'))
            || url.origin().ascii_serialization() != value
        {
            return Err(InvalidBrowserOrigin);
        }
        Ok(Self(url))
    }
}

impl BrowserOrigin {
    fn authority(&self) -> &str {
        &self.0[url::Position::BeforeHost..url::Position::AfterPort]
    }

    fn matches_host(&self, host: &str) -> bool {
        let default_port = match self.0.scheme() {
            "https" => ":443",
            _ => ":80",
        };
        let host = if self.0.port().is_none() {
            host.strip_suffix(default_port).unwrap_or(host)
        } else {
            host
        };
        self.authority().eq_ignore_ascii_case(host)
    }
}

/// The local address comes from the accepted socket, so wildcard listeners
/// trust the actual destination address, never arbitrary DNS names.
pub(crate) struct Access<'a> {
    local: SocketAddr,
    extra: &'a [BrowserOrigin],
}

impl<'a> Access<'a> {
    pub(crate) fn new(local: SocketAddr, extra: &'a [BrowserOrigin]) -> Self {
        Self { local, extra }
    }

    fn local_authority(&self, authority: &str) -> bool {
        let authority = if self.local.port() == 80 {
            authority.strip_suffix(":80").unwrap_or(authority)
        } else {
            authority
        };
        let ip = match self.local {
            SocketAddr::V4(addr) => addr.ip().to_string(),
            SocketAddr::V6(addr) => format!("[{}]", addr.ip()),
        };
        let port = if self.local.port() == 80 {
            String::new()
        } else {
            format!(":{}", self.local.port())
        };
        authority.eq_ignore_ascii_case(&format!("{ip}{port}"))
            || (self.local.ip().is_loopback()
                && authority.eq_ignore_ascii_case(&format!("localhost{port}")))
    }

    pub(crate) fn allows_host(&self, headers: &HeaderMap) -> bool {
        let Some(host) = unique(headers, header::HOST).and_then(|value| value.to_str().ok()) else {
            return false;
        };
        self.local_authority(host) || self.extra.iter().any(|origin| origin.matches_host(host))
    }

    pub(crate) fn allows_origin(&self, headers: &HeaderMap) -> bool {
        let Some(origin) = unique(headers, header::ORIGIN)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<BrowserOrigin>().ok())
        else {
            // This is a browser bridge. Native clients use the daemon protocol;
            // they do not receive an Origin-free bypass into the human session.
            return false;
        };
        if self.extra.contains(&origin) {
            return true;
        }
        // Default access is same-origin: localhost and the numeric address are
        // individually trusted, but are not aliases across the browser boundary.
        origin.0.scheme() == "http"
            && self.local_authority(origin.authority())
            && unique(headers, header::HOST)
                .and_then(|host| host.to_str().ok())
                .is_some_and(|host| origin.matches_host(host))
    }
}

fn unique(headers: &HeaderMap, name: header::HeaderName) -> Option<&HeaderValue> {
    let mut values = headers.get_all(name).iter();
    let first = values.next()?;
    values.next().is_none().then_some(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_are_exact_browser_serializations() {
        for value in [
            "http://localhost:5173",
            "https://review.example",
            "http://[::1]:9777",
        ] {
            assert!(value.parse::<BrowserOrigin>().is_ok(), "{value}");
        }
        for value in [
            "null",
            "*",
            "http://*.example",
            "http://localhost:5173/",
            "http://ada:secret@localhost:5173",
            "http://localhost:5173?query",
            "http://localhost:5173#fragment",
            "file://localhost",
            "ws://localhost:9777",
            "http://localhost:5173 http://other.example",
            " http://localhost:5173",
            "http://localhost:80",
        ] {
            assert!(value.parse::<BrowserOrigin>().is_err(), "{value}");
        }
    }

    #[test]
    fn host_policy_uses_the_actual_destination_for_ipv4_ipv6_and_default_ports() {
        for (local, host, origin) in [
            ("127.0.0.1:80", "127.0.0.1:80", "http://127.0.0.1"),
            ("127.0.0.1:80", "LOCALHOST:80", "http://localhost"),
            ("[::1]:9777", "[::1]:9777", "http://[::1]:9777"),
            ("[::1]:9777", "localhost:9777", "http://localhost:9777"),
            ("192.0.2.1:9777", "192.0.2.1:9777", "http://192.0.2.1:9777"),
            (
                "[2001:db8::1]:9777",
                "[2001:db8::1]:9777",
                "http://[2001:db8::1]:9777",
            ),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(header::HOST, host.parse().unwrap());
            headers.insert(header::ORIGIN, origin.parse().unwrap());
            let access = Access::new(local.parse().unwrap(), &[]);
            assert!(access.allows_host(&headers), "{local} {host}");
            assert!(access.allows_origin(&headers), "{local} {origin}");
            headers.insert(header::HOST, "rebound.example:9777".parse().unwrap());
            assert!(!access.allows_host(&headers));
            assert!(!access.allows_origin(&headers));
        }
        let access = Access::new("192.0.2.1:9777".parse().unwrap(), &[]);
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, "localhost:9777".parse().unwrap());
        assert!(!access.allows_host(&headers));
    }
}
