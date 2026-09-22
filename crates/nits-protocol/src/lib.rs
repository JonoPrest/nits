//! Nits wire protocol.
//!
//! Every type that crosses a process boundary — daemon ⇄ client, client-core ⇄
//! UI — lives here. The crate is wasm-safe: no I/O, no clocks, no threads.
//!
//! Conventions (see `AGENTS.md`): enums with payloads are
//! `#[serde(tag = "type")]`; enums whose variants are all unit serialise as a
//! bare `PascalCase` string (`"Open"`, not `{"type":"Open"}`); structs
//! and enums are `deny_unknown_fields`, ids are newtypes, invariants are
//! enforced by constructors. Example values live in `nits-protocol-fixtures`
//! (dev tooling only); `cargo xtask fixtures` writes them under
//! `fixtures/protocol/` and that crate's tests assert every variant has one.

pub mod creation;
pub mod deferral;
pub mod discovery;
pub mod domain;
pub mod events;
pub mod fetch;
pub mod ids;
pub mod invariants;
pub mod lifecycle;
pub mod reference;
pub mod render;
pub mod replay;
pub mod revision;
pub mod rpc;
#[cfg(feature = "schema")]
mod schema;
pub mod suggestion;
pub mod version;

pub use creation::*;
pub use deferral::*;
pub use discovery::*;
pub use domain::*;
pub use events::*;
pub use fetch::*;
pub use ids::*;
pub use invariants::*;
pub use lifecycle::*;
pub use reference::*;
pub use render::*;
pub use replay::*;
pub use revision::*;
pub use rpc::*;
pub use suggestion::*;
pub use version::*;
