//! MCP server for Nits (plan 2.5). Speaks JSON-RPC 2.0 over newline-delimited
//! stdio (the MCP stdio transport) and proxies review operations to a `nitsd`
//! daemon over its unix socket or WebSocket. Session identity tools configure
//! the author presented by this adapter's daemon connection.
//!
//! Every mutation is attributed to [`nits_protocol::Author::Agent`], built
//! from the MCP client's `initialize` info and subsequent session identity
//! updates, so provenance is structural.

pub mod checkpoint;
pub mod jsonrpc;
pub mod management;
pub mod server;
mod supervisor;
pub mod tools;
mod worker;

pub use server::{Endpoint, Server};

use std::collections::HashMap;

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::task::{AbortHandle, JoinSet};

use jsonrpc::{Outgoing, RequestId};
use server::Dispatch;

/// Bound per-session daemon connections and pending response tasks.
pub const MAX_EVENT_WAITS: usize = 32;

/// Serve MCP on this process's stdin/stdout until stdin closes.
///
/// Context resolution belongs to the caller: launch flags override the persisted
/// default. Calls establish subscriptions and change session state in input order;
/// acknowledged event waits alone may overlap later calls.
pub async fn serve_stdio(
    endpoint: Endpoint,
    identity: server::AgentIdentity,
    build: nits_protocol::BuildInfo,
) -> anyhow::Result<()> {
    supervisor::serve(
        Server::new(endpoint, identity, build).checkpoint(),
        BufReader::new(nitsd::launch::stdio::InputPump::stdin()?),
        tokio::io::stdout(),
    )
    .await
}

/// Serve a newline-delimited MCP connection. EOF or an I/O error cancels and joins
/// all event waits. The generic streams also let tests exercise real interleavings
/// through exactly the same scheduler as stdio.
pub async fn serve(
    mut server: Server,
    input: impl AsyncBufRead + Unpin,
    mut output: impl AsyncWrite + Unpin,
) -> anyhow::Result<()> {
    let mut lines = input.lines();
    let mut tasks = JoinSet::new();
    let mut active: HashMap<RequestId, AbortHandle> = HashMap::new();
    let result = async {
        loop {
            tokio::select! {
                line = lines.next_line() => {
                    let Some(line) = line? else { break; };
                    if line.trim().is_empty() { continue; }
                    match server.dispatch_line(&line).await {
                        Dispatch::Notification => {},
                        Dispatch::Cancel(id) => {
                            if let Some(task) = active.remove(&id) {
                                // MCP cancellation is a notification: no response
                                // for it or the cancelled request.
                                task.abort();
                            }
                        },
                        Dispatch::Reply(reply) => write_reply(&mut output, &reply).await?,
                        Dispatch::Wait { id, wait } => {
                            let error = if active.contains_key(&id) {
                                Some("request id already has an outstanding event wait")
                            } else if active.len() >= MAX_EVENT_WAITS {
                                Some("at most 32 event waits may be outstanding; cancel or finish one first")
                            } else { None };
                            if let Some(message) = error {
                                write_reply(&mut output, &Outgoing::error(id.value(), jsonrpc::INVALID_REQUEST, message)).await?;
                            } else {
                                let key = id.clone();
                                let task = tasks.spawn(async move {
                                    let reply = wait.reply(&id).await;
                                    (id, reply)
                                });
                                active.insert(key, task);
                            }
                        },
                    }
                },
                completed = tasks.join_next_with_id(), if !tasks.is_empty() => {
                    match completed {
                    Some(Ok((task_id, (id, reply)))) => {
                        // A cancelled task may finish just before abort. Its old
                        // reply must not remove or complete a reused request ID.
                        if active.get(&id).is_some_and(|task| task.id() == task_id) {
                            active.remove(&id);
                            write_reply(&mut output, &reply).await?;
                        }
                    }
                    Some(Err(error)) if !error.is_cancelled() => return Err(error.into()),
                    Some(Err(_)) | None => {},
                    }
                },
            }
        }
        Ok(())
    }.await;
    tasks.shutdown().await;
    result
}

async fn write_reply(
    output: &mut (impl AsyncWrite + Unpin),
    reply: &Outgoing,
) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec(reply)?;
    bytes.push(b'\n');
    output.write_all(&bytes).await?;
    output.flush().await?;
    Ok(())
}

/// Private CLI worker entry point. The parent supplies a validated checkpoint
/// before any application I/O; host stdout stays owned by the supervisor.
pub async fn serve_worker_stdio(build: nits_protocol::BuildInfo) -> anyhow::Result<()> {
    worker::serve(
        build,
        BufReader::new(nitsd::launch::stdio::InputPump::stdin()?),
        tokio::io::stdout(),
    )
    .await
}
