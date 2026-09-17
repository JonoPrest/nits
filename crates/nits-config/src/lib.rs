//! Client configuration shared by the CLI, MCP shim and desktop app: a set
//! of named **contexts** (where a daemon is and how to reach it). Stored as
//! TOML at `$XDG_CONFIG_HOME/nits/config.toml` (default
//! `~/.config/nits/config.toml`).
//!
//! `current_context` supplies the default for new processes. Explicit
//! `--context`/`NITS_CONTEXT` selections override it, followed by the implicit
//! [`DEFAULT_CONTEXT`]. Running MCP sessions retain their own selection.
//!
//! ```toml
//! [contexts.laptop]
//! type = "Local"
//!
//! [contexts.build-box]
//! type = "Ssh"
//! host = "build-box"
//!
//! [contexts.shared]
//! type = "Ws"
//! url = "ws://reviews.internal:7677"
//! ```
//!
//! Daemon lifecycle per kind: `Local` and `Ssh` contexts start the daemon on
//! demand (locally, or via `ssh host nits daemon stdio` which does it remotely);
//! a `Ws` context is somebody else's daemon and is only connected to.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Name of the context used when none is configured.
pub const DEFAULT_CONTEXT: &str = "local";

/// A nonempty context name, with no surrounding whitespace or controls.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ContextName(String);

impl ContextName {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<str> for ContextName {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl TryFrom<String> for ContextName {
    type Error = ConfigError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.is_empty() || value.trim() != value || value.chars().any(char::is_control) {
            return Err(ConfigError::InvalidName);
        }
        Ok(Self(value))
    }
}

impl std::str::FromStr for ContextName {
    type Err = ConfigError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_from(value.to_owned())
    }
}

impl From<ContextName> for String {
    fn from(name: ContextName) -> Self {
        name.0
    }
}

impl std::fmt::Display for ContextName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Why this process selected a context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SelectionOrigin {
    Flag,
    Environment,
    Persisted,
    Implicit,
    AdHoc,
    Mcp,
}

/// A resolved context and the selection that led to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Selection {
    pub name: ContextName,
    pub context: Context,
    pub origin: SelectionOrigin,
}

/// Transport identity reported with MCP results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ContextKind {
    Local,
    Ssh,
    Ws,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("context names must be nonempty, with no surrounding whitespace or control characters")]
    InvalidName,
    #[error(
        "context {0} is the persisted default; select another with `nits context use` before removing it"
    )]
    CurrentContext(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("serialize: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("no context named {0}")]
    NoSuchContext(String),
    #[error("HOME is not set")]
    NoHome,
}

/// Which binary an ssh context runs on the remote, parsed from the two
/// wire spellings once, here. The rest of the code never sees "both keys"
/// or has to decide which wins: the states a config can express are the
/// variants, and nothing else.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum RemoteBin {
    /// No key: `nits` from the remote PATH.
    #[default]
    Default,
    /// `bin = "..."`: where `nits` is on that host.
    Nits(String),
    /// `nitsd = "..."`, written before the daemon became `nits daemon
    /// serve`. Its own variant because it must never be *run* — the value
    /// names an executable that does not understand `daemon stdio` — only
    /// refused, with the edit to make.
    Legacy(String),
}

impl RemoteBin {
    /// The remote `nits` to run, for a context that may be run at all.
    #[must_use]
    pub fn nits(&self) -> Option<&str> {
        match self {
            RemoteBin::Default => Some("nits"),
            RemoteBin::Nits(bin) => Some(bin),
            RemoteBin::Legacy(_) => None,
        }
    }
}

/// Where a daemon is and how to reach it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ContextWire", into = "ContextWire")]
pub enum Context {
    /// A daemon on this machine, started on demand.
    Local {
        /// Default: `$XDG_DATA_HOME/nits` or `~/.local/share/nits`.
        data_dir: Option<PathBuf>,
        /// Default: `<data_dir>/nitsd.sock`.
        socket: Option<PathBuf>,
    },
    /// A daemon on another machine, reached by `ssh <host> nits daemon
    /// stdio`, which starts it there if needed. Auth, jumps and ports come
    /// from `~/.ssh/config`.
    Ssh {
        host: String,
        bin: RemoteBin,
        /// Extra arguments for the remote daemon, e.g. `--data-dir`.
        args: Vec<String>,
        /// The ssh client to run. Default: `ssh`.
        ssh: Option<String>,
    },
    /// A daemon already listening for WebSocket clients.
    Ws { url: String },
}

/// The file's spelling of a context. Only [`Context`] is used past the
/// boundary; this exists so the two ways of naming a remote binary are
/// turned into one domain value — or rejected — exactly once.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum ContextWire {
    Local {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        data_dir: Option<PathBuf>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        socket: Option<PathBuf>,
    },
    Ssh {
        host: String,
        /// The remote `nits` binary. Default: `nits` on the remote PATH.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bin: Option<String>,
        /// The pre-one-binary key, recognised so a config written before
        /// the daemon became `nits daemon serve` can be reported rather
        /// than silently mis-run.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        nitsd: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ssh: Option<String>,
    },
    Ws {
        url: String,
    },
}

/// Why a context in the file is not a context.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContextParseError {
    #[error(
        "context for {host} sets both `bin` and `nitsd`; {}",
        Context::LEGACY_NITSD_HELP
    )]
    BothBinAndNitsd { host: String },
}

impl TryFrom<ContextWire> for Context {
    type Error = ContextParseError;

    fn try_from(w: ContextWire) -> Result<Self, Self::Error> {
        Ok(match w {
            ContextWire::Local { data_dir, socket } => Context::Local { data_dir, socket },
            ContextWire::Ssh {
                host,
                bin,
                nitsd,
                args,
                ssh,
            } => {
                let bin = match (bin, nitsd) {
                    (Some(_), Some(_)) => {
                        return Err(ContextParseError::BothBinAndNitsd { host });
                    }
                    (Some(bin), None) => RemoteBin::Nits(bin),
                    (None, Some(nitsd)) => RemoteBin::Legacy(nitsd),
                    (None, None) => RemoteBin::Default,
                };
                Context::Ssh {
                    host,
                    bin,
                    args,
                    ssh,
                }
            }
            ContextWire::Ws { url } => Context::Ws { url },
        })
    }
}

impl From<Context> for ContextWire {
    fn from(c: Context) -> Self {
        match c {
            Context::Local { data_dir, socket } => ContextWire::Local { data_dir, socket },
            Context::Ssh {
                host,
                bin,
                args,
                ssh,
            } => {
                let (bin, nitsd) = match bin {
                    RemoteBin::Default => (None, None),
                    RemoteBin::Nits(bin) => (Some(bin), None),
                    RemoteBin::Legacy(nitsd) => (None, Some(nitsd)),
                };
                ContextWire::Ssh {
                    host,
                    bin,
                    nitsd,
                    args,
                    ssh,
                }
            }
            Context::Ws { url } => ContextWire::Ws { url },
        }
    }
}

impl Context {
    /// What to tell someone whose config still has the old key.
    pub const LEGACY_NITSD_HELP: &'static str = "the daemon is now `nits daemon serve`. Replace `nitsd = \"...\"` with \
         `bin = \"nits\"` (or the path to `nits` on that host), or drop the line \
         to use `nits` from the remote PATH";

    #[must_use]
    pub fn kind(&self) -> ContextKind {
        match self {
            Self::Local { .. } => ContextKind::Local,
            Self::Ssh { .. } => ContextKind::Ssh,
            Self::Ws { .. } => ContextKind::Ws,
        }
    }

    /// One-line description for listings.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Context::Local { data_dir, socket } => match (data_dir, socket) {
                (None, None) => "local".into(),
                (d, s) => format!(
                    "local{}{}",
                    d.as_ref()
                        .map(|d| format!(" data_dir={}", d.display()))
                        .unwrap_or_default(),
                    s.as_ref()
                        .map(|s| format!(" socket={}", s.display()))
                        .unwrap_or_default()
                ),
            },
            Context::Ssh { host, .. } => format!("ssh {host}"),
            Context::Ws { url } => format!("ws {url}"),
        }
    }
}

/// The whole config file. Unknown top-level keys are ignored (not denied)
/// so a file written by a newer or older client still loads; contexts
/// themselves stay strict.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_context: Option<ContextName>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub contexts: BTreeMap<ContextName, Context>,
}

impl Config {
    /// `$XDG_CONFIG_HOME/nits/config.toml` or `~/.config/nits/config.toml`.
    pub fn default_path() -> Result<PathBuf, ConfigError> {
        if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
            return Ok(PathBuf::from(x).join("nits").join("config.toml"));
        }
        let home = std::env::var("HOME").map_err(|_| ConfigError::NoHome)?;
        Ok(PathBuf::from(home).join(".config/nits/config.toml"))
    }

    /// Read `path`; a missing file is an empty config.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|source| ConfigError::Parse {
                path: path.to_path_buf(),
                source,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Atomically replace `path`, creating parent directories. Readers see
    /// either complete file, and a failed write leaves the old file intact.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        std::fs::create_dir_all(parent)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        if let Ok(metadata) = std::fs::metadata(path) {
            temporary
                .as_file()
                .set_permissions(metadata.permissions())?;
        }
        temporary.write_all(toml::to_string_pretty(self)?.as_bytes())?;
        temporary.as_file().sync_all()?;
        temporary.persist(path).map_err(|error| error.error)?;
        Ok(())
    }

    /// Resolve the persisted default unless an explicit name overrides it.
    pub fn selection(
        &self,
        explicit: Option<(&ContextName, SelectionOrigin)>,
    ) -> Result<Selection, ConfigError> {
        let (name, origin) = match explicit {
            Some((name, origin)) => (name.clone(), origin),
            None => match &self.current_context {
                Some(name) => (name.clone(), SelectionOrigin::Persisted),
                None => (DEFAULT_CONTEXT.parse()?, SelectionOrigin::Implicit),
            },
        };
        let (_, context) = self.resolve(Some(name.as_str()))?;
        Ok(Selection {
            name,
            context,
            origin,
        })
    }

    /// Resolve an explicit name, the persisted selection, or [`DEFAULT_CONTEXT`].
    /// An unconfigured `local` is implicit, so a fresh install needs no file.
    pub fn resolve(&self, name: Option<&str>) -> Result<(String, Context), ConfigError> {
        let name = name
            .or_else(|| self.current_context.as_ref().map(ContextName::as_str))
            .unwrap_or(DEFAULT_CONTEXT);
        if let Some(c) = self.contexts.get(name) {
            return Ok((name.to_string(), c.clone()));
        }
        if name == DEFAULT_CONTEXT {
            return Ok((
                name.to_string(),
                Context::Local {
                    data_dir: None,
                    socket: None,
                },
            ));
        }
        Err(ConfigError::NoSuchContext(name.to_string()))
    }

    pub fn remove(&mut self, name: &str) -> Result<Context, ConfigError> {
        if self
            .current_context
            .as_ref()
            .is_some_and(|current| current.as_str() == name)
        {
            return Err(ConfigError::CurrentContext(name.to_owned()));
        }
        self.contexts
            .remove(name)
            .ok_or_else(|| ConfigError::NoSuchContext(name.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_resolves() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nits/config.toml");
        let empty = Config::load(&path).unwrap();
        assert_eq!(empty, Config::default());
        let (name, ctx) = empty.resolve(None).unwrap();
        assert_eq!(name, "local");
        assert!(matches!(ctx, Context::Local { .. }));
        assert!(empty.resolve(Some("nope")).is_err());

        let mut cfg = Config::default();
        cfg.contexts.insert(
            "box".parse().unwrap(),
            Context::Ssh {
                host: "build-box".into(),
                bin: RemoteBin::Default,
                args: vec!["--data-dir".into(), "/srv/nits".into()],
                ssh: None,
            },
        );
        cfg.contexts.insert(
            "shared".parse().unwrap(),
            Context::Ws {
                url: "ws://h:7677".into(),
            },
        );
        cfg.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("type = \"Ssh\""), "{text}");
        let back = Config::load(&path).unwrap();
        assert_eq!(back, cfg);
        assert_eq!(back.resolve(Some("box")).unwrap().0, "box");
        // With no persisted selection, the implicit local context wins.
        assert_eq!(back.resolve(None).unwrap().0, "local");

        let mut back = back;
        back.remove("box").unwrap();
        assert!(back.resolve(Some("box")).is_err());
        assert!(Config::load(&path).is_ok());
    }

    #[test]
    fn persisted_selection_round_trips_and_explicit_selection_wins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut config = Config::default();
        config.contexts.insert(
            "remote".parse().unwrap(),
            Context::Ws {
                url: "ws://review.example:7677".into(),
            },
        );
        config.current_context = Some("remote".parse().unwrap());
        config.save(&path).unwrap();
        let mut loaded = Config::load(&path).unwrap();
        assert_eq!(
            loaded.selection(None).unwrap().origin,
            SelectionOrigin::Persisted
        );
        assert_eq!(loaded.resolve(None).unwrap().0, "remote");
        let local = "local".parse().unwrap();
        let explicit = loaded
            .selection(Some((&local, SelectionOrigin::Flag)))
            .unwrap();
        assert_eq!(explicit.name, local);
        assert_eq!(explicit.origin, SelectionOrigin::Flag);
        assert!(matches!(
            loaded.remove("remote"),
            Err(ConfigError::CurrentContext(_))
        ));
        loaded.current_context = Some(local);
        loaded.remove("remote").unwrap();
        loaded.save(&path).unwrap();
        assert_eq!(
            Config::load(&path).unwrap().resolve(None).unwrap().0,
            "local"
        );
        loaded.current_context = Some("missing".parse().unwrap());
        assert!(matches!(
            loaded.resolve(None),
            Err(ConfigError::NoSuchContext(_))
        ));
        assert_eq!(loaded.resolve(Some("local")).unwrap().0, "local");
    }

    #[test]
    fn atomic_save_preserves_permissions_and_leaves_no_temporary_files() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "# previous config").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        Config::default().save(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o640
        );
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        // A destination that cannot be replaced leaves the existing entry intact.
        let destination = dir.path().join("directory");
        std::fs::create_dir(&destination).unwrap();
        assert!(Config::default().save(&destination).is_err());
        assert!(destination.is_dir());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn context_names_are_validated_at_the_boundary() {
        for invalid in ["", " ", " remote", "remote ", "a\nb", "a\0b"] {
            assert!(invalid.parse::<ContextName>().is_err());
        }
        assert_eq!(
            "build-box".parse::<ContextName>().unwrap().as_str(),
            "build-box"
        );
        assert!(toml::from_str::<Config>("current_context = ' '").is_err());
    }

    #[test]
    fn context_map_keys_are_validated_when_decoding_the_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for name in ["", " broken ", "bad\\nname"] {
            std::fs::write(&path, format!("[contexts.\"{name}\"]\ntype = \"Local\"\n")).unwrap();
            let error = Config::load(&path).unwrap_err();
            assert!(matches!(error, ConfigError::Parse { .. }));
            assert!(error.to_string().contains("context names must be nonempty"));
        }
        std::fs::write(&path, "[contexts.build-box]\ntype = \"Local\"\n").unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.contexts.keys().next().unwrap().as_str(), "build-box");
        config.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), config);
    }

    /// A config written before the daemon became `nits daemon serve` still
    /// loads — it has to, or the CLI could not even tell the user what is
    /// wrong — but the old `nitsd` key stays distinguishable from `bin` so
    /// callers refuse it instead of running a binary that cannot serve.
    #[test]
    fn the_pre_one_binary_nitsd_key_is_recognised_not_silently_reused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[contexts.box]\ntype = \"Ssh\"\nhost = \"build-box\"\nnitsd = \"/opt/bin/nitsd\"\n",
        )
        .unwrap();

        let cfg = Config::load(&path).expect("an old config still loads");
        let (_, ctx) = cfg.resolve(Some("box")).unwrap();
        // Recognised as the legacy key, and crucially not adopted as the
        // binary to run: `/opt/bin/nitsd daemon stdio` is not a command
        // the old daemon understands. There is no state in which a caller
        // has to decide between two spellings — the parse already did.
        assert!(matches!(
            &ctx,
            Context::Ssh { bin: RemoteBin::Legacy(n), .. } if n == "/opt/bin/nitsd"
        ));
        assert_eq!(
            match &ctx {
                Context::Ssh { bin, .. } => bin.nits(),
                Context::Local { .. } | Context::Ws { .. } => None,
            },
            None,
            "a legacy context names nothing runnable"
        );
        assert!(Context::LEGACY_NITSD_HELP.contains("bin = "));
        // It round-trips back to the old spelling rather than being
        // rewritten into something the old client would not understand.
        let round: Context = toml::from_str(&toml::to_string(&ctx).unwrap()).unwrap();
        assert_eq!(round, ctx);

        // Both keys at once is not a context: the file is refused, with
        // the edit to make, rather than one of them silently winning.
        std::fs::write(
            &path,
            "[contexts.box]\ntype = \"Ssh\"\nhost = \"build-box\"\nbin = \"nits\"\nnitsd = \"/opt/bin/nitsd\"\n",
        )
        .unwrap();
        let err = Config::load(&path).expect_err("both keys is not a context");
        let text = err.to_string();
        assert!(text.contains("bin") && text.contains("nitsd"), "{text}");

        // A migrated config has no legacy key and names `nits`.
        std::fs::write(
            &path,
            "[contexts.box]\ntype = \"Ssh\"\nhost = \"build-box\"\nbin = \"nits\"\n",
        )
        .unwrap();
        let (_, ctx) = Config::load(&path).unwrap().resolve(Some("box")).unwrap();
        assert!(matches!(
            &ctx,
            Context::Ssh { bin: RemoteBin::Nits(b), .. } if b == "nits"
        ));
        // And one with neither key runs `nits` from the remote PATH.
        std::fs::write(
            &path,
            "[contexts.box]\ntype = \"Ssh\"\nhost = \"build-box\"\n",
        )
        .unwrap();
        let (_, ctx) = Config::load(&path).unwrap().resolve(Some("box")).unwrap();
        assert!(matches!(
            &ctx,
            Context::Ssh {
                bin: RemoteBin::Default,
                ..
            }
        ));
        assert_eq!(
            match &ctx {
                Context::Ssh { bin, .. } => bin.nits(),
                Context::Local { .. } | Context::Ws { .. } => None,
            },
            Some("nits")
        );
    }
}
