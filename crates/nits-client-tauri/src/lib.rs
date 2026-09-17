//! Tauri wrapper around `nits-client-host` (PLAN 4.3, ARCHITECTURE §6.2).
//!
//! The webview calls three commands — `dispatch {action}`, `key {chord}`,
//! `attach {}` — which forward to [`nits_client_host::Handle`]; a task
//! drains the host's patch receiver into `app.emit("view", patches)`.
//! Nothing here knows about the review model: the host owns the core.

use std::path::{Path, PathBuf};

use nits_client_core::{Action, IdSeed, KeyChord, ViewPatch};
use nits_client_host::{Handle, HostConfig, Identity, KvConfig};
use nits_config::{Config, Context};
use nits_protocol::{Author, BuildInfo, ClientId};
use nitsd::contexts::{ContextError, DaemonEndpoint, StartPolicy};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio_util::sync::CancellationToken;

/// The event the UI listens on; payload is `Vec<ViewPatch>`.
pub const VIEW_EVENT: &str = "view";

#[derive(Debug, thiserror::Error)]
pub enum SetupError {
    #[error("config: {0}")]
    Config(#[from] nits_config::ConfigError),
    #[error("{what}: {source}")]
    Io {
        what: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("host: {0}")]
    Host(#[from] nits_client_host::HostError),
    #[error("context: {0}")]
    Context(#[from] ContextError),
    #[error("host task exited during setup")]
    HostGone,
}

/// Shared with every command: the host handle.
#[derive(Debug)]
pub struct Host {
    handle: Handle,
}

/// Error reported to the webview when the host task has ended.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct HostGone;

impl std::fmt::Display for HostGone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("host task has exited")
    }
}

// `tauri::command` requires `State` by value.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
fn dispatch(host: State<'_, Host>, action: serde_json::Value) -> Result<(), HostGone> {
    host.handle
        .dispatch_json(action)
        .then_some(())
        .ok_or(HostGone)
}

#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
fn key(host: State<'_, Host>, chord: KeyChord) -> Result<(), HostGone> {
    host.handle.key(chord).then_some(()).ok_or(HostGone)
}

/// The webview reports adapter failures here so they reach the Rust log.
#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
fn client_error(message: String) {
    tracing::warn!(%message, "webview");
}

#[allow(clippy::needless_pass_by_value)]
#[tauri::command]
fn attach(host: State<'_, Host>) -> Result<(), HostGone> {
    host.handle.attach().then_some(()).ok_or(HostGone)
}

/// Who this desktop client is: `$USER@hostname`, a fresh client id.
#[must_use]
pub fn identity() -> Identity {
    let machine = gethostname::gethostname().to_string_lossy().into_owned();
    let name = std::env::var("USER").unwrap_or_else(|_| "anonymous".into());
    let (ts, r) = nitsd::ids::fresh_parts();
    Identity {
        client_id: ClientId::from_parts(ts, r),
        client: BuildInfo {
            name: "nits-desktop".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        author: Author::Human { name, machine },
    }
}

/// Where the desktop gets its context definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointSource {
    Named {
        context: Option<String>,
        config: Option<PathBuf>,
    },
    Local {
        data_dir: Option<PathBuf>,
        socket: Option<PathBuf>,
    },
    WebSocket {
        url: String,
    },
}

/// Fully typed endpoint selection received at the desktop process boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointOptions {
    pub source: EndpointSource,
    pub start: StartPolicy,
}

/// Resolve the selected source into the endpoint every host connection
/// attempt will dial.
pub fn endpoint_for(options: &EndpointOptions) -> Result<DaemonEndpoint, SetupError> {
    let context = match &options.source {
        EndpointSource::Named { context, config } => {
            let config_path = match config {
                Some(path) => path.clone(),
                None => Config::default_path()?,
            };
            let cfg = Config::load(&config_path)?;
            let (_, context) = cfg.resolve(context.as_deref())?;
            context
        }
        EndpointSource::Local { data_dir, socket } => Context::Local {
            data_dir: data_dir.clone(),
            socket: socket.clone(),
        },
        EndpointSource::WebSocket { url } => Context::Ws { url: url.clone() },
    };
    Ok(DaemonEndpoint::resolve(&context, options.start)?)
}

/// Host config for the desktop: redb KV under `app_data_dir`, random seed.
#[must_use]
pub fn host_config(endpoint: DaemonEndpoint, app_data_dir: &Path) -> HostConfig {
    nits_client_host::host_config(
        endpoint,
        identity(),
        IdSeed(fastrand::u128(..)),
        KvConfig::Redb(app_data_dir.join("kv.redb")),
    )
}

/// Start the host and forward its patches to the webview. Returns the
/// state the commands read.
pub fn start_host(app: &AppHandle, config: HostConfig) -> Result<Host, SetupError> {
    if let KvConfig::Redb(p) = &config.kv
        && let Some(dir) = p.parent()
    {
        std::fs::create_dir_all(dir).map_err(|source| SetupError::Io {
            what: "app data dir",
            source,
        })?;
    }
    let shutdown = CancellationToken::new();
    // `spawn` calls `tokio::spawn`; `setup` runs on the main thread, outside
    // Tauri's runtime, so enter it explicitly.
    let (handle, mut patches) = {
        let rt = tauri::async_runtime::handle();
        let _guard = rt.inner().enter();
        nits_client_host::spawn(config, shutdown.clone())?
    };
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(batch) = patches.recv().await {
            tracing::debug!(patches = ?batch, "view");
            if let Err(e) = app.emit(VIEW_EVENT, &batch) {
                tracing::warn!(error = %e, "emit view patches");
            }
        }
        shutdown.cancel();
    });
    // The core only dials when asked (`Action::Connect`); the desktop
    // always wants to be connected.
    if !handle.dispatch(Action::Connect) {
        return Err(SetupError::HostGone);
    }
    Ok(Host { handle })
}

/// Build and run the app with a context selection fixed for this process.
pub fn run(options: EndpointOptions) -> Result<(), Box<dyn std::error::Error>> {
    tauri::Builder::default()
        .setup(move |app| {
            let data_dir = app.path().app_data_dir()?;
            let endpoint = endpoint_for(&options)?;
            let host = start_host(app.handle(), host_config(endpoint, &data_dir))?;
            app.manage(host);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            dispatch,
            key,
            attach,
            client_error
        ])
        .run(tauri::generate_context!())?;
    Ok(())
}

/// Patches are the same type the host emits; kept public so a test can
/// assert the event payload shape.
pub type Patches = Vec<ViewPatch>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_a_human_on_this_machine() {
        let id = identity();
        assert_eq!(id.client.name, "nits-desktop");
        assert!(matches!(id.author, Author::Human { .. }));
    }

    #[test]
    fn host_config_puts_kv_under_app_data_dir() {
        let endpoint = DaemonEndpoint::WebSocket {
            url: "ws://review.example:7677".into(),
        };
        let cfg = host_config(endpoint.clone(), Path::new("/data"));
        assert_eq!(cfg.endpoint, endpoint);
        assert!(matches!(&cfg.kv, KvConfig::Redb(p) if p == Path::new("/data/kv.redb")));
    }

    #[test]
    fn endpoint_options_preserve_ad_hoc_sources_and_lifecycle() {
        let websocket = endpoint_for(&EndpointOptions {
            source: EndpointSource::WebSocket {
                url: "ws://review.example:7677".into(),
            },
            start: StartPolicy::RequireRunning,
        })
        .unwrap();
        assert_eq!(
            websocket,
            DaemonEndpoint::WebSocket {
                url: "ws://review.example:7677".into()
            }
        );

        let local = endpoint_for(&EndpointOptions {
            source: EndpointSource::Local {
                data_dir: Some(PathBuf::from("/tmp/nits-data")),
                socket: Some(PathBuf::from("/tmp/nits.sock")),
            },
            start: StartPolicy::RequireRunning,
        })
        .unwrap();
        assert!(matches!(
            local,
            DaemonEndpoint::Local {
                start: StartPolicy::RequireRunning,
                ..
            }
        ));
    }

    /// The `view` payload is exactly the array `CoreTauri.res` parses.
    #[test]
    fn view_payload_is_a_patch_array() {
        let patches: Patches = Vec::new();
        assert_eq!(serde_json::to_string(&patches).unwrap(), "[]");
    }
}
