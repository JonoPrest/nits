//! Nits review core: event store, git engine, diff render model, anchoring.
//!
//! Everything here is daemon-only. Public functions take and return
//! `nits-protocol` types so every transport is a thin adapter.

#![deny(clippy::wildcard_enum_match_arm)]

pub mod anchor;
mod checkpoints;
pub mod comments;
pub mod core;
mod directory;
pub mod git;
pub mod patch;
pub mod render;
mod replay;
mod repository;
pub mod review;
pub mod store;
mod suggestion_file;

pub use core::{Core, CoreError, Ctx, DataDir};

pub use nits_protocol as protocol;
