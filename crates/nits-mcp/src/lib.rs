//! MCP server for Nits (plan 2.5). Speaks JSON-RPC 2.0 over newline-delimited
//! stdio (the MCP stdio transport) and proxies review operations to a `nitsd`
//! daemon over its unix socket or WebSocket. Session identity tools configure
//! the author presented by this adapter's daemon connection.
//!
//! Every mutation is attributed to [`nits_protocol::Author::Agent`], built
//! from the MCP client's `initialize` info and subsequent session identity
//! updates, so provenance is structural.

pub mod jsonrpc;
pub mod server;
pub mod tools;

pub use server::{Endpoint, Server};

/// Serve MCP on this process's stdin/stdout until stdin closes.
///
/// The entry point behind `nits mcp`. Context resolution belongs to the
/// caller, so the CLI's global `--context`/`--socket`/`--ws` flags govern
/// the MCP server exactly as they govern every other subcommand.
pub async fn serve_stdio(
    endpoint: Endpoint,
    identity: server::AgentIdentity,
    build: nits_protocol::BuildInfo,
) -> anyhow::Result<()> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let mut server = Server::new(endpoint, identity, build);
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = server.handle_line(&line).await {
            let mut bytes = serde_json::to_vec(&reply)?;
            bytes.push(b'\n');
            out.write_all(&bytes).await?;
            out.flush().await?;
        }
    }
    Ok(())
}
