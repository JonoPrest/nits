//! Bounded reconnect for the read-only CLI event stream. Each printed page is
//! flushed before its scanned cursor advances; a retry keeps the same fixed
//! historical window and never repeats a CLI mutation.

use std::time::Duration;

use nits_protocol::{Author, BuildInfo, ClientId, ProtocolVersion, RpcError};
use nitsd::client::{Client, ClientError, Identity};
use nitsd::codec::CodecError;
use nitsd::contexts::{self, ContextError, DaemonEndpoint};
use nitsd::ops::{Ops, OpsError};

#[derive(Debug)]
pub(crate) struct EventConnection {
    endpoint: DaemonEndpoint,
    author: Author,
    build: BuildInfo,
}

impl EventConnection {
    pub(crate) fn new(cli: &super::Cli, context: &nits_config::Context) -> anyhow::Result<Self> {
        let (build, author) = super::principal(cli);
        Ok(Self {
            endpoint: DaemonEndpoint::resolve(context, super::client_start_policy(cli)?)?,
            author,
            build,
        })
    }

    pub(crate) async fn reconnect(&self, deadline: tokio::time::Instant) -> anyhow::Result<Ops> {
        let mut delay = Duration::from_millis(100);
        let mut last_error = String::from("connection closed during restart");
        loop {
            tokio::time::sleep_until((tokio::time::Instant::now() + delay).min(deadline)).await;
            let attempt = async {
                let (read, write) = contexts::dial(&self.endpoint).await?.into_parts();
                let (ts, random) = nitsd::ids::fresh_parts();
                Client::handshake_framed(
                    read,
                    write,
                    Identity {
                        client_id: ClientId::from_parts(ts, random),
                        client: self.build.clone(),
                        author: self.author.clone(),
                    },
                    ProtocolVersion::CURRENT,
                )
                .await
                .map(Ops::new)
                .map_err(ContextError::from)
            };
            match tokio::time::timeout_at(deadline, attempt).await {
                Ok(Ok(ops)) => return Ok(ops),
                Ok(Err(ContextError::Client(error))) if !retry_client(&error) => {
                    anyhow::bail!(
                        "event stream could not reconnect: {error}. If the daemon was upgraded, restart this stream with the installed Nits client; the printed event cursor remains valid."
                    )
                }
                Ok(Err(error)) => last_error = error.to_string(),
                Err(_) => break,
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            delay = (delay * 2).min(Duration::from_secs(2));
        }
        anyhow::bail!(
            "event stream did not reconnect within 60 seconds: {last_error}. Inspect daemon upgrade-status; resume from the last printed event cursor."
        )
    }
}

pub(crate) fn retry_page(error: &OpsError) -> bool {
    match error {
        OpsError::Client(error) => retry_client(error),
        OpsError::Rpc(error) => retry_rpc(error),
        OpsError::Invalid(_) | OpsError::Shape => false,
    }
}

fn retry_client(error: &ClientError) -> bool {
    match error {
        ClientError::Closed | ClientError::Codec(CodecError::Io(_)) => true,
        ClientError::Rejected(error) | ClientError::Rpc(error) => retry_rpc(error),
        ClientError::BadHandshake(_)
        | ClientError::Codec(CodecError::Json(_) | CodecError::Oversized { .. }) => false,
    }
}

fn retry_rpc(error: &RpcError) -> bool {
    matches!(
        error,
        RpcError::Restarting { .. } | RpcError::RestartInterrupted { .. }
    )
}
