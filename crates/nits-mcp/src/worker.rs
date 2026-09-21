//! Private, versioned supervisor/worker transport. A ticket belongs to a worker
//! generation, independently of the host's reusable JSON-RPC request IDs.

use std::collections::HashMap;
use std::num::NonZeroU64;

use nits_protocol::{BuildDescriptor, BuildInfo, WorkerVersion};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};
use tokio::task::{AbortHandle, JoinSet};

use crate::checkpoint::SessionCheckpoint;
use crate::jsonrpc::{Incoming, Outgoing, RequestId};
use crate::server::{Dispatch, Server};
use crate::tools::ToolContract;

/// Monotonic private identity. Reusing a host ID cannot collect an old reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct Ticket(NonZeroU64);

impl Ticket {
    pub(crate) const FIRST: Self = Self(NonZeroU64::MIN);

    pub(crate) fn next(self) -> Option<Self> {
        self.0
            .get()
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .map(Self)
    }

    pub(crate) fn request_id(self) -> RequestId {
        RequestId::Number(self.0.get().into())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub(crate) enum Input {
    Restore {
        version: WorkerVersion,
        checkpoint: SessionCheckpoint,
    },
    Dispatch {
        ticket: Ticket,
        message: Incoming,
    },
    Cancel {
        ticket: Ticket,
    },
    Shutdown {},
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Output {
    pub checkpoint: SessionCheckpoint,
    pub event: OutputEvent,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub(crate) enum OutputEvent {
    Canceled {
        ticket: Ticket,
    },
    Ready {
        version: WorkerVersion,
        build: BuildDescriptor,
        tools: Vec<ToolContract>,
    },
    Handled {
        ticket: Ticket,
        reply: Option<Outgoing>,
    },
}

pub(crate) async fn write(
    output: &mut (impl AsyncWrite + Unpin),
    message: &impl Serialize,
) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec(message)?;
    bytes.push(b'\n');
    output.write_all(&bytes).await?;
    output.flush().await?;
    Ok(())
}

async fn handled(
    output: &mut (impl AsyncWrite + Unpin),
    ticket: Ticket,
    server: &Server,
    reply: Option<Outgoing>,
) -> anyhow::Result<()> {
    write(
        output,
        &Output {
            checkpoint: server.checkpoint(),
            event: OutputEvent::Handled { ticket, reply },
        },
    )
    .await
}

/// The initial message restores a disconnected session. Ready is sent before
/// any application Hello or configuration reload, making candidate preflight
/// independent of the daemon version it will eventually use.
pub(crate) async fn serve(
    build: BuildInfo,
    input: impl AsyncBufRead + Unpin,
    mut output: impl AsyncWrite + Unpin,
) -> anyhow::Result<()> {
    let mut lines = input.lines();
    let first = lines
        .next_line()
        .await?
        .ok_or_else(|| anyhow::anyhow!("worker ended before session restore"))?;
    let Input::Restore {
        version,
        checkpoint,
    } = serde_json::from_str(&first)?
    else {
        anyhow::bail!("worker requires session restore before requests");
    };
    anyhow::ensure!(
        version == WorkerVersion::CURRENT,
        "unsupported supervisor/worker protocol"
    );
    let mut server = Server::from_checkpoint(checkpoint, build)?;
    write(
        &mut output,
        &Output {
            checkpoint: server.checkpoint(),
            event: OutputEvent::Ready {
                version: WorkerVersion::CURRENT,
                build: nitsd::build::running()?,
                tools: crate::tools::contracts(),
            },
        },
    )
    .await?;
    let mut tasks = JoinSet::new();
    let mut active: HashMap<Ticket, AbortHandle> = HashMap::new();
    let result = async {
        loop {
            tokio::select! {
                line = lines.next_line() => {
                    let Some(line) = line? else { break; };
                    match serde_json::from_str::<Input>(&line)? {
                        Input::Restore { .. } => anyhow::bail!("worker session may only be restored once"),
                        Input::Shutdown {} => break,
                        Input::Cancel { ticket } => {
                            if let Some(task) = active.remove(&ticket) { task.abort(); }
                            write(&mut output, &Output {checkpoint: server.checkpoint(), event: OutputEvent::Canceled {ticket}}).await?;
                        }
                        Input::Dispatch { ticket, mut message } => {
                            // The private ticket is the daemon scheduler's RPC ID;
                            // only the supervisor knows the external host ID.
                            if message.id.is_some() { message.id = Some(ticket.request_id().value()); }
                            match server.dispatch(message).await {
                                Dispatch::Reply(reply) => handled(&mut output, ticket, &server, Some(reply)).await?,
                                Dispatch::Notification => handled(&mut output, ticket, &server, None).await?,
                                Dispatch::Cancel(_) => anyhow::bail!("worker cancellation requires a private ticket"),
                                Dispatch::Wait { id, wait } => {
                                    if active.len() >= crate::MAX_EVENT_WAITS {
                                        let reply = Outgoing::error(id.value(), crate::jsonrpc::INVALID_REQUEST, "at most 32 event waits may be outstanding; cancel or finish one first");
                                        handled(&mut output, ticket, &server, Some(reply)).await?;
                                    } else if let std::collections::hash_map::Entry::Vacant(entry) = active.entry(ticket) {
                                        let task = tasks.spawn(async move { (ticket, wait.reply(&id).await) });
                                        entry.insert(task);
                                    } else {
                                        anyhow::bail!("duplicate private worker ticket");
                                    }
                                }
                            }
                        }
                    }
                }
                completed = tasks.join_next_with_id(), if !tasks.is_empty() => {
                    match completed {
                        Some(Ok((task_id, (ticket, reply)))) => {
                            if active.get(&ticket).is_some_and(|task|task.id() == task_id) {
                                active.remove(&ticket);
                                handled(&mut output, ticket, &server, Some(reply)).await?;
                            }
                        }
                        Some(Err(error)) if !error.is_cancelled() => return Err(error.into()),
                        Some(Err(_)) | None => {},
                    }
                }
            }
        }
        Ok(())
    }.await;
    tasks.shutdown().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{BufReader, DuplexStream, Lines, ReadHalf, WriteHalf};

    fn initial(socket: &std::path::Path) -> SessionCheckpoint {
        Server::new(
            crate::Endpoint {
                selection: nits_config::Selection {
                    name: "fixture".parse().unwrap(),
                    context: nits_config::Context::Local {
                        data_dir: None,
                        socket: Some(socket.to_owned()),
                    },
                    origin: nits_config::SelectionOrigin::Flag,
                },
                config_path: socket.with_extension("toml"),
                start: nitsd::contexts::StartPolicy::RequireRunning,
            },
            crate::server::AgentIdentity {
                model: "fixture-model".into(),
                session_id: "fixture-session".into(),
                invoked_by: None,
            },
            BuildInfo {
                name: "fixture".into(),
                version: "1".into(),
            },
        )
        .checkpoint()
    }

    async fn next(output: &mut Lines<BufReader<ReadHalf<DuplexStream>>>) -> Output {
        let line = tokio::time::timeout(std::time::Duration::from_secs(3), output.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        serde_json::from_str(&line).unwrap()
    }

    async fn send(
        input: &mut WriteHalf<DuplexStream>,
        ticket: Ticket,
        method: &str,
        params: serde_json::Value,
    ) {
        write(
            input,
            &Input::Dispatch {
                ticket,
                message: Incoming {
                    jsonrpc: "2.0".into(),
                    id: Some(serde_json::json!("external-id")),
                    method: method.into(),
                    params,
                },
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn worker_preflight_has_no_daemon_io_and_checkpoints_before_reply() {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("daemon.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let checkpoint = initial(&socket);
        let (host, child) = tokio::io::duplex(128 * 1024);
        let (reader, mut input) = tokio::io::split(host);
        let mut output = BufReader::new(reader).lines();
        let (reader, writer) = tokio::io::split(child);
        let task = tokio::spawn(serve(
            BuildInfo {
                name: "fixture".into(),
                version: "2".into(),
            },
            BufReader::new(reader),
            writer,
        ));
        write(
            &mut input,
            &Input::Restore {
                version: WorkerVersion::CURRENT,
                checkpoint: checkpoint.clone(),
            },
        )
        .await
        .unwrap();
        let Output {
            checkpoint: restored,
            event: OutputEvent::Ready { tools, .. },
        } = next(&mut output).await
        else {
            panic!("expected readiness")
        };
        assert_eq!(restored, checkpoint);
        assert_eq!(tools, crate::tools::contracts());
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), listener.accept())
                .await
                .is_err(),
            "restore must not negotiate application Hello"
        );
        drop(listener);
        std::fs::remove_file(&socket).unwrap();
        let ticket = Ticket::FIRST;
        send(
            &mut input,
            ticket,
            "initialize",
            serde_json::json!({"clientInfo":{"name":"worker-reviewer"}}),
        )
        .await;
        let Output {
            checkpoint,
            event:
                OutputEvent::Handled {
                    ticket: returned,
                    reply: Some(reply),
                },
        } = next(&mut output).await
        else {
            panic!("expected initialized checkpoint")
        };
        assert_eq!(returned, ticket);
        assert_eq!(reply.id, ticket.request_id().value());
        assert!(checkpoint.initialized());
        assert!(reply.error.is_none());
        assert_eq!(
            reply.result.unwrap()["capabilities"]["tools"]["listChanged"],
            true
        );
        let wire = serde_json::to_value(checkpoint).unwrap();
        assert_eq!(wire["state"]["author"]["name"], "worker-reviewer");
        assert_eq!(wire["state"]["author"]["session_id"], "fixture-session");
        write(&mut input, &Input::Cancel { ticket }).await.unwrap();
        assert!(
            matches!(next(&mut output).await.event, OutputEvent::Canceled { ticket: canceled } if canceled == ticket)
        );
        let next_ticket = ticket.next().unwrap();
        send(&mut input, next_ticket, "ping", serde_json::json!({})).await;
        let OutputEvent::Handled {
            ticket,
            reply: Some(reply),
        } = next(&mut output).await.event
        else {
            panic!("expected ping")
        };
        assert_eq!(ticket, next_ticket);
        assert_eq!(reply.id, next_ticket.request_id().value());
        write(&mut input, &Input::Shutdown {}).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}
