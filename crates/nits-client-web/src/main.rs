//! `nits-web [context] [--port 9777]`: WebSocket bridge for the browser
//! UI. Run `pnpm --dir ui dev`, open the Vite URL, and the page connects
//! to this bridge via its `/ws` proxy. Trust the dev origin explicitly with
//! `--allow-origin http://localhost:5173` (use the exact URL Vite prints).

use std::net::{Ipv4Addr, SocketAddr};

use clap::Parser;
use nits_client_core::IdSeed;
use nits_client_host::KvConfig;
use nits_config::Config;
use nits_protocol_shim::{Author, BuildInfo};
use nitsd::contexts::{DaemonEndpoint, StartPolicy};

// The wire types come through nits-client-core's re-export so the bin
// needs no direct nits-protocol dependency.
mod nits_protocol_shim {
    pub use nits_client_core::protocol::{Author, BuildInfo};
}

#[derive(Debug, Parser)]
#[command(about = "WebSocket bridge around nits-client-host for the browser UI")]
struct Args {
    /// Named context from the config file. Default: `local`.
    context: Option<String>,
    /// Port to listen on (loopback only).
    #[arg(long, default_value_t = 9777)]
    port: u16,
    /// Trust an additional exact UI origin, e.g. `http://localhost:5173` for Vite.
    /// May be repeated. No wildcards, paths, or trailing slash.
    #[arg(long = "allow-origin")]
    allowed_origins: Vec<nits_client_web::BrowserOrigin>,
}

fn author() -> Author {
    let machine = gethostname::gethostname().to_string_lossy().into_owned();
    let name = std::env::var("USER").unwrap_or_else(|_| "anonymous".into());
    Author::Human { name, machine }
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let args = Args::parse();
    let cfg = Config::load(&Config::default_path()?)?;
    let (name, ctx) = cfg.resolve(args.context.as_deref())?;
    let endpoint = DaemonEndpoint::resolve(&ctx, StartPolicy::StartIfNeeded)?;
    // Dev tool: memory KV is enough (prefs reset per run).
    let mut config = nits_client_web::web_config(
        endpoint,
        BuildInfo {
            name: "nits-web".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        author(),
        IdSeed(fastrand::u128(..)),
        KvConfig::Memory,
    );
    config.allowed_origins = args.allowed_origins;
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, args.port));
    let server = nits_client_web::serve(addr, config).await?;
    eprintln!(
        "nits-web: ws://{} (context {name}: {})",
        server.addr(),
        ctx.describe()
    );
    tokio::signal::ctrl_c().await?;
    server.stop();
    Ok(())
}
