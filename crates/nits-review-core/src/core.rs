//! The `Core` façade: one type composing store, git, render and anchoring.
//! Every transport (unix, ws, mcp, cli) is a thin adapter over this.
//!
//! `Core` performs no clock or id generation of its own: callers pass a
//! [`Ctx`] carrying who is acting, when, and the client-side sequence. Ids
//! for created entities come from the client (see §5.2 optimistic creation).

use std::path::PathBuf;
use std::sync::Mutex;

use nits_protocol::{Author, ClientId, ClientSeq, EntityKind, Event, EventBody, Seq, Timestamp};

use crate::git::GitError;
use crate::render::Highlighter;
use crate::render::cache::{CacheError, RenderCache};
use crate::repository::RepositoryRegistry;
use crate::store::{NewEvent, Store, StoreError};

/// Who is acting, from where, and when. Built by the transport per request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ctx {
    pub author: Author,
    pub client_id: ClientId,
    pub client_seq: ClientSeq,
    pub now: Timestamp,
}

#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("{kind:?} {id} not found")]
    NotFound { kind: EntityKind, id: String },
    #[error("invalid request: {reason}")]
    Invalid { reason: String },
    #[error("forbidden: {reason}")]
    Forbidden { reason: String },
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Git(#[from] GitError),
    #[error(transparent)]
    Cache(#[from] CacheError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl CoreError {
    pub(crate) fn not_found(kind: EntityKind, id: &impl ToString) -> Self {
        CoreError::NotFound {
            kind,
            id: id.to_string(),
        }
    }
    pub(crate) fn invalid(reason: impl Into<String>) -> Self {
        CoreError::Invalid {
            reason: reason.into(),
        }
    }
    pub(crate) fn forbidden(reason: impl Into<String>) -> Self {
        CoreError::Forbidden {
            reason: reason.into(),
        }
    }
}

/// Files under the data dir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataDir {
    pub root: PathBuf,
}

impl DataDir {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }
    #[must_use]
    pub fn state(&self) -> PathBuf {
        self.root.join("state.redb")
    }
    #[must_use]
    pub fn render_cache(&self) -> PathBuf {
        self.root.join("render-cache.redb")
    }
}

pub struct Core {
    pub(crate) store: Store,
    pub(crate) cache: RenderCache,
    pub(crate) hl: Highlighter,
    pub(crate) repositories: Mutex<RepositoryRegistry>,
}

impl std::fmt::Debug for Core {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Core")
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}

impl Core {
    /// Open (or create) everything under `data_dir`.
    pub fn open(data_dir: &DataDir) -> Result<Self, CoreError> {
        std::fs::create_dir_all(&data_dir.root)?;
        Ok(Self {
            store: Store::open(&data_dir.state())?,
            cache: RenderCache::open(&data_dir.render_cache())?,
            hl: Highlighter::new(),
            repositories: Mutex::new(RepositoryRegistry::default()),
        })
    }

    pub(crate) fn append(&self, ctx: &Ctx, body: EventBody) -> Result<Event, CoreError> {
        Ok(self.store.append(NewEvent {
            ts: ctx.now,
            author: ctx.author.clone(),
            client_id: ctx.client_id,
            client_seq: ctx.client_seq,
            body,
        })?)
    }

    // ---- log access -------------------------------------------------------

    pub fn events_after(&self, after: Option<Seq>) -> Result<Vec<Event>, CoreError> {
        Ok(self.store.events_after(after)?)
    }

    pub fn last_seq(&self) -> Result<Option<Seq>, CoreError> {
        Ok(self.store.last_seq()?)
    }
}
