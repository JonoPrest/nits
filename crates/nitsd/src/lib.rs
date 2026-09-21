//! `nitsd`: the Nits daemon. Serves `nits-review-core` over length-prefixed
//! JSON frames on a unix socket (or stdio). See `docs/ARCHITECTURE.md` §4.8
//! and §4.9.
//!
//! Layers, innermost first:
//!
//! - [`codec`]: frames ⇄ bytes. Knows nothing about message types.
//! - [`handshake`]: `Hello` → `Welcome`/`Rejected`, as a typestate.
//! - [`daemon`]: `Core` behind a single writer thread plus a blocking pool for
//!   reads; the event broadcast every connection tails.
//! - [`connection`]: one client: request mux, cancellation, subscriptions.
//! - [`server`]: accept loops (unix socket, stdio).
//! - [`serve`]: running the daemon, and the stdio proxy that reaches it.
//! - [`client`]: the async client used by tests, the CLI and the MCP shim.

mod admission;
pub mod build;
pub mod client;
pub mod codec;
pub mod connection;
pub mod contexts;
pub mod control;
pub mod daemon;
pub mod dispatch;
pub mod handshake;
pub mod ids;
pub mod launch;
pub mod ops;
pub mod ownership;
pub mod render_text;
pub mod serve;
pub mod server;
pub mod transport;
pub mod upgrade;
pub mod watcher;

pub use daemon::Daemon;
