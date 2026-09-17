//! `nits`: the whole tool in one binary (plan 2.6) — client, daemon
//! (`daemon serve`) and MCP server (`mcp`). Every client subcommand is a
//! printer over [`nitsd::ops::Ops`]; `--json` prints the protocol values
//! verbatim for scripting.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, bail};
use clap::{Args, FromArgMatches, Parser, Subcommand, ValueEnum};
use nits_config::{Context, ContextName, Selection, SelectionOrigin};
use nits_protocol::{
    AgentVia, Anchor, Author, BuildInfo, ClientId, CommentKind, DirectoryReviewOutcome, Event,
    EventBody, LineNo, LineRange, Mutation, NonEmpty, RefSpec, RenderOpts, RepoId, RepoPath,
    Review, ReviewId, ReviewTarget, Seq, Side, Since, SubscribeScope, ThreadId, Workspace,
    WorkspaceId,
};
use nitsd::client::Identity;
use nitsd::contexts::{self, Status};
use nitsd::ops::Ops;
use nitsd::render_text;
use serde::Serialize;
use std::fmt::Write as _;

#[derive(Debug, Parser)]
#[command(name = "nits", version, about)]
struct Cli {
    /// Named context from the config file (see `nits context`). Default:
    /// the default saved by `nits context use`, then implicit `local`.
    /// This flag or `NITS_CONTEXT` overrides the saved default.
    #[arg(long, short = 'c', env = "NITS_CONTEXT", global = true)]
    context: Option<ContextName>,
    /// Config file. Default: `$XDG_CONFIG_HOME/nits/config.toml`.
    #[arg(long, env = "NITS_CONFIG", global = true)]
    config: Option<PathBuf>,
    /// Ad-hoc local context: this daemon socket. Overrides `--context`.
    #[arg(long, env = "NITS_SOCKET", global = true)]
    socket: Option<PathBuf>,
    /// Ad-hoc context: a daemon WebSocket URL (`ws://host:port`).
    /// `--ws` remains accepted as a deprecated alias.
    #[arg(long, alias = "ws", env = "NITS_WS_URL", global = true, value_parser = parse_daemon_url)]
    daemon_url: Option<String>,
    /// Workspace for review create/list and events. Default for reviews:
    /// the workspace whose attached repo contains the current directory.
    #[arg(long, global = true, value_name = "ID")]
    workspace: Option<WorkspaceId>,
    /// Ad-hoc local context: data dir (socket at `<data-dir>/nitsd.sock`).
    #[arg(long, env = "NITS_DATA_DIR", global = true)]
    data_dir: Option<PathBuf>,
    /// Whether a connection may start a managed daemon when needed.
    #[arg(long, value_enum, default_value_t = StartPolicyArg::StartIfNeeded, global = true)]
    start_policy: StartPolicyArg,
    /// Port for the served web UI (default: a free port).
    #[arg(long, env = "NITS_PORT", default_value_t = 0)]
    port: u16,
    /// Print protocol values as JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    /// Your name for attribution. Default: `$USER`.
    #[arg(long, env = "NITS_USER", global = true)]
    user: Option<String>,
    /// Act as this agent (attribution `Agent{via: Cli}`) instead of a human.
    #[arg(long, env = "NITS_AGENT", global = true)]
    agent: Option<String>,
    /// With no subcommand: serve the browser UI until Ctrl-C, vite-style.
    /// A path (`nits .`) opens (or creates) that directory's review; no
    /// path opens the workspace menu. `--headless` only ensures the
    /// review exists (e.g. `nits -c hetzner ~/proj --headless` on a
    /// remote context); `--ui desktop` launches the Tauri app instead.
    #[arg(value_name = "PATH")]
    path: Option<PathBuf>,
    #[arg(long, value_enum, default_value_t = Ui::Web, conflicts_with = "headless")]
    ui: Ui,
    /// Shorthand for `--ui headless`.
    #[arg(long)]
    headless: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

/// How bare `nits` presents the review. A configurable default is future
/// work; `Tui` is reserved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Ui {
    Web,
    Desktop,
    Headless,
}

/// CLI spelling of the daemon lifecycle policy. The transport-level enum
/// stays independent of clap; this adapter converts once at the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum StartPolicyArg {
    StartIfNeeded,
    RequireRunning,
}

impl StartPolicyArg {
    fn as_arg(self) -> &'static str {
        match self {
            Self::StartIfNeeded => "start-if-needed",
            Self::RequireRunning => "require-running",
        }
    }
}

impl From<StartPolicyArg> for contexts::StartPolicy {
    fn from(value: StartPolicyArg) -> Self {
        match value {
            StartPolicyArg::StartIfNeeded => Self::StartIfNeeded,
            StartPolicyArg::RequireRunning => Self::RequireRunning,
        }
    }
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Named daemons to talk to (local, ssh, websocket), like kubectl contexts.
    #[command(subcommand)]
    Context(ContextCmd),
    /// Start, stop or inspect the current context's daemon.
    #[command(subcommand)]
    Daemon(DaemonCmd),
    /// Serve the Model Context Protocol on stdin/stdout, for agents. The
    /// global context flags choose the daemon, as everywhere else.
    Mcp,
    /// Workspaces and their repos.
    #[command(subcommand)]
    Workspace(WorkspaceCmd),
    /// Reviews.
    #[command(subcommand)]
    Review(ReviewCmd),
    /// Changed files in a review.
    Files {
        #[command(flatten)]
        review: ReviewArg,
    },
    /// Diff of one changed file.
    Diff {
        review: ReviewId,
        path: String,
        #[arg(long)]
        repo: Option<RepoId>,
        /// Ignore whitespace.
        #[arg(short = 'w', long)]
        ignore_whitespace: bool,
        /// Context lines.
        #[arg(short = 'U', long, default_value_t = 3)]
        context_lines: u32,
    },
    /// A whole file at the review's head (or base).
    Show {
        review: ReviewId,
        path: String,
        #[arg(long)]
        repo: Option<RepoId>,
        #[arg(long, value_enum, default_value_t = SideArg::Head)]
        side: SideArg,
    },
    /// Comments and threads.
    #[command(subcommand)]
    Comment(CommentCmd),
    /// The keys config: generate defaults, print the schema, check a file.
    #[command(subcommand)]
    Keys(KeysCmd),
    /// Print events; `--follow` keeps waiting for more.
    Events {
        #[arg(long)]
        follow: bool,
        #[arg(long)]
        review: Option<ReviewId>,
        /// Only `ReviewRequested` events addressed to this agent name.
        #[arg(long)]
        awaiting: Option<String>,
        /// Replay everything after this log position first.
        #[arg(long)]
        since: Option<u64>,
    },
}

#[derive(Debug, Subcommand)]
enum ContextCmd {
    /// Configured contexts.
    List,
    /// The selected context's name and details (`-c`, `NITS_CONTEXT`, or
    /// the persisted default, or implicit `local`), including selection origin.
    Show,
    /// Save a named context as the default for new CLI and MCP processes.
    Use {
        name: ContextName,
    },
    /// A daemon on this machine.
    AddLocal {
        name: String,
        #[arg(long)]
        data_dir: Option<PathBuf>,
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// A daemon on another machine via `ssh HOST nits daemon stdio`.
    AddSsh {
        name: String,
        /// Host as understood by your ssh config (`user@host`, alias).
        host: String,
        /// Remote `nits` binary. Default: `nits` on the remote PATH.
        /// No `--nitsd` alias on purpose: that flag named a binary which
        /// cannot serve `daemon stdio`, so accepting it would write a
        /// context that fails on first use.
        #[arg(long = "bin")]
        bin: Option<String>,
        /// Extra arguments for the remote daemon, e.g. `--data-dir /x`.
        #[arg(long = "arg")]
        args: Vec<String>,
    },
    /// A daemon already listening for WebSocket clients.
    AddWs {
        name: String,
        url: String,
    },
    Remove {
        name: String,
    },
}

#[derive(Debug, Subcommand)]
enum DaemonCmd {
    /// Whether the daemon is running (every context with `--all`).
    Status {
        #[arg(long)]
        all: bool,
    },
    /// Start the daemon if it is not running.
    Start,
    /// Ask the daemon to exit; it restarts on the next connection.
    Stop,
    /// Be the daemon: serve this context's socket in the foreground.
    ///
    /// Hidden because nothing needs to type it — `nits` starts daemons
    /// itself, with exactly this command (see `nitsd::launch`).
    #[command(hide = true)]
    Serve(ServeArgs),
    /// Pipe stdin/stdout to this machine's daemon, starting one if nothing
    /// answers; `--start-policy require-running` exits 3 instead. What
    /// `ssh host nits daemon stdio` runs for an ssh context.
    #[command(hide = true)]
    Stdio(ServeArgs),
}

/// Flags shared by `daemon serve` and `daemon stdio`. Where to listen comes
/// from the global context flags; these say how to behave once listening.
#[derive(Debug, Args)]
struct ServeArgs {
    /// Exit after this many seconds with no client connected.
    #[arg(long, env = "NITS_IDLE_EXIT")]
    idle_exit: Option<u64>,
    /// Also listen for WebSocket clients on this address, e.g.
    /// `127.0.0.1:7677`. Off unless given.
    ///
    /// `--ws-listen` binds the server; global `--daemon-url` selects a client
    /// endpoint. Their field names must differ too: clap keys arguments by ID.
    /// `nitsd::launch::WS_LISTEN_FLAG` is the same flag, for spawning.
    #[arg(long, env = "NITS_WS")]
    ws_listen: Option<std::net::SocketAddr>,
}

#[derive(Debug, Subcommand)]
enum WorkspaceCmd {
    /// Create a workspace; prints its id.
    Add {
        name: String,
    },
    List,
    /// Attach a git repository to a workspace; prints the repo id.
    Attach {
        #[arg(value_name = "WORKSPACE")]
        workspace_id: WorkspaceId,
        /// Default: the current directory.
        path: Option<PathBuf>,
        /// Display name. Default: the directory name.
        #[arg(long)]
        name: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum ReviewCmd {
    /// Create a review; prints its id.
    Create {
        /// Base ref: branch name, tag:NAME, full commit oid, `HEAD`,
        /// `upstream`, or `worktree`.
        #[arg(long)]
        base: String,
        /// Head ref, same forms as `--base`.
        #[arg(long)]
        head: String,
        /// Repo to review. Default: the one containing the current
        /// directory, else the workspace's only repo.
        #[arg(long)]
        repo: Option<RepoId>,
        #[arg(long)]
        title: Option<String>,
    },
    List,
    /// Review, targets, files and thread counts.
    Show {
        #[command(flatten)]
        review: ReviewArg,
    },
}

#[derive(Debug, Subcommand)]
enum CommentCmd {
    /// Start a thread on the review, a file, or a line range.
    Add(AddComment),
    Reply {
        review: ReviewId,
        thread: ThreadId,
        #[arg(long)]
        body: String,
    },
    Resolve {
        review: ReviewId,
        thread: ThreadId,
    },
    List {
        #[command(flatten)]
        review: ReviewArg,
    },
}

#[derive(Debug, Args)]
struct AddComment {
    #[command(flatten)]
    review: ReviewArg,
    #[arg(long)]
    body: String,
    /// Anchor to this file (whole file unless `--line`/`--lines`).
    #[arg(long)]
    path: Option<String>,
    #[arg(long)]
    repo: Option<RepoId>,
    #[arg(long, value_enum, default_value_t = SideArg::Head)]
    side: SideArg,
    /// One line.
    #[arg(long, conflicts_with = "lines", value_parser = parse_line)]
    line: Option<LineNo>,
    /// Inclusive range, `START-END` or `START:END` (line numbers start at 1).
    #[arg(long, value_parser = parse_lines)]
    lines: Option<LineRange>,
    /// Attach a unified-diff suggestion the reviewer can apply.
    #[arg(long)]
    patch: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum SideArg {
    Base,
    Head,
}

impl From<SideArg> for Side {
    fn from(s: SideArg) -> Self {
        match s {
            SideArg::Base => Side::Base,
            SideArg::Head => Side::Head,
        }
    }
}

/// clap accepts two spellings at the boundary, but commands receive exactly
/// one validated review ID. The required, exclusive group rejects ambiguity.
/// Use this only when review is the sole positional: making it optional before
/// another positional would change how interspersed flags bind paths/threads.
#[derive(Debug)]
struct ReviewArg(ReviewId);

impl Args for ReviewArg {
    fn augment_args(cmd: clap::Command) -> clap::Command {
        cmd.arg(
            clap::Arg::new("review_positional")
                .value_name("REVIEW")
                .help("Review ID (or pass --review <REVIEW>)")
                .value_parser(clap::value_parser!(ReviewId)),
        )
        .arg(
            clap::Arg::new("review_option")
                .long("review")
                .value_name("REVIEW")
                .help("Review ID (alternative to the positional argument)")
                .value_parser(clap::value_parser!(ReviewId)),
        )
        .group(
            clap::ArgGroup::new("review_selection")
                .args(["review_positional", "review_option"])
                .required(true),
        )
    }

    fn augment_args_for_update(cmd: clap::Command) -> clap::Command {
        Self::augment_args(cmd)
    }
}

impl FromArgMatches for ReviewArg {
    fn from_arg_matches(matches: &clap::ArgMatches) -> Result<Self, clap::Error> {
        matches
            .get_one::<ReviewId>("review_positional")
            .or_else(|| matches.get_one::<ReviewId>("review_option"))
            .copied()
            .map(Self)
            .ok_or_else(|| {
                clap::Error::raw(
                    clap::error::ErrorKind::MissingRequiredArgument,
                    "pass the review ID positionally or with --review <REVIEW>",
                )
            })
    }

    fn update_from_arg_matches(&mut self, matches: &clap::ArgMatches) -> Result<(), clap::Error> {
        *self = Self::from_arg_matches(matches)?;
        Ok(())
    }
}

fn parse_line(s: &str) -> Result<LineNo, String> {
    let value = s.parse().map_err(|e| format!("line number: {e}"))?;
    LineNo::new(value).ok_or_else(|| "lines start at 1".into())
}

fn parse_lines(s: &str) -> Result<LineRange, String> {
    let (a, b) = s
        .split_once([':', '-'])
        .ok_or_else(|| "expected START-END or START:END (inclusive)".to_string())?;
    LineRange::new(parse_line(a)?, parse_line(b)?).map_err(|e| e.to_string())
}

fn parse_daemon_url(s: &str) -> Result<String, String> {
    if s.parse::<WorkspaceId>().is_ok() {
        return Err(format!(
            "this looks like a workspace ID; use --workspace {s} (for example: nits --workspace {s} review list); --daemon-url expects ws://host:port or wss://host:port"
        ));
    }
    if !s.starts_with("ws://") && !s.starts_with("wss://") {
        return Err("expected a daemon WebSocket URL: ws://host:port or wss://host:port".into());
    }
    Ok(s.into())
}

/// `worktree` / `upstream` / `HEAD` / `tag:NAME` / 40-hex commit / branch.
fn parse_ref(s: &str) -> anyhow::Result<RefSpec> {
    Ok(match s {
        "worktree" | "wt" | "working-tree" => RefSpec::WorkingTree,
        "upstream" | "@{upstream}" | "@{u}" => RefSpec::Upstream,
        "HEAD" => RefSpec::Head,
        _ => {
            if let Some(tag) = s.strip_prefix("tag:") {
                RefSpec::Tag { name: tag.into() }
            } else if s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
                RefSpec::Commit { oid: s.parse()? }
            } else {
                RefSpec::Branch { name: s.into() }
            }
        }
    })
}

fn ref_label(reference: &RefSpec) -> String {
    match reference {
        RefSpec::Branch { name } => name.clone(),
        RefSpec::Commit { oid } => oid.to_string(),
        RefSpec::Tag { name } => format!("tag:{name}"),
        RefSpec::WorkingTree => "worktree".into(),
        RefSpec::Upstream => "upstream".into(),
        RefSpec::Head => "HEAD".into(),
    }
}

/// The context to use: ad-hoc flags beat `--context` beats the config.
fn resolve_context(
    cli: &Cli,
    cfg: &nits_config::Config,
    origin: SelectionOrigin,
) -> anyhow::Result<Selection> {
    if let Some(url) = &cli.daemon_url {
        return Ok(Selection {
            name: "--daemon-url".parse()?,
            context: Context::Ws { url: url.clone() },
            origin: SelectionOrigin::AdHoc,
        });
    }
    if cli.socket.is_some() || cli.data_dir.is_some() {
        return Ok(Selection {
            name: (if cli.socket.is_some() {
                "--socket"
            } else {
                "--data-dir"
            })
            .parse()?,
            context: Context::Local {
                data_dir: cli.data_dir.clone(),
                socket: cli.socket.clone(),
            },
            origin: SelectionOrigin::AdHoc,
        });
    }
    Ok(cfg.selection(cli.context.as_ref().map(|name| (name, origin)))?)
}

fn config_path(cli: &Cli) -> anyhow::Result<PathBuf> {
    match &cli.config {
        Some(p) => Ok(p.clone()),
        None => Ok(nits_config::Config::default_path()?),
    }
}

/// Typed desktop arguments equivalent to this process's context selection.
/// The desktop parses them into its own endpoint-source enum before use.
fn desktop_args(cli: &Cli) -> Vec<std::ffi::OsString> {
    let mut args = vec!["--start-policy".into(), cli.start_policy.as_arg().into()];
    if let Some(url) = &cli.daemon_url {
        args.extend(["--ws".into(), url.into()]);
    } else if cli.socket.is_some() || cli.data_dir.is_some() {
        if let Some(socket) = &cli.socket {
            args.extend(["--socket".into(), socket.as_os_str().into()]);
        }
        if let Some(data_dir) = &cli.data_dir {
            args.extend(["--data-dir".into(), data_dir.as_os_str().into()]);
        }
    } else {
        if let Some(context) = &cli.context {
            args.push(context.as_str().into());
        }
        if let Some(config) = &cli.config {
            args.extend(["--config".into(), config.as_os_str().into()]);
        }
    }
    args
}

fn identity(cli: &Cli) -> Identity {
    let (client, author) = principal(cli);
    let (ts, r) = nitsd::ids::fresh_parts();
    Identity {
        client_id: ClientId::from_parts(ts, r),
        client,
        author,
    }
}

fn principal(cli: &Cli) -> (BuildInfo, Author) {
    let machine = gethostname::gethostname().to_string_lossy().into_owned();
    let name = cli
        .user
        .clone()
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_else(|| "anonymous".into());
    let author = match &cli.agent {
        Some(agent) => Author::Agent {
            name: agent.clone(),
            model: std::env::var("NITS_AGENT_MODEL").unwrap_or_else(|_| "unknown".into()),
            session_id: std::env::var("NITS_SESSION_ID").unwrap_or_default(),
            invoked_by: Some(nits_protocol::Human { name, machine }),
            via: AgentVia::Cli,
        },
        None => Author::Human { name, machine },
    };
    (
        BuildInfo {
            name: "nits".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
        author,
    )
}

async fn connect(cli: &Cli, ctx: &Context) -> anyhow::Result<Ops> {
    let client = contexts::connect(ctx, identity(cli), cli.start_policy.into())
        .await
        .with_context(|| format!("connecting to {}", ctx.describe()))?;
    Ok(Ops::new(client))
}

/// The daemon's own logs go to stderr, which `spawn_detached` points at
/// `<data-dir>/nitsd.log`. `RUST_LOG` steers it; off by default.
fn init_daemon_logging() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
}

/// The paths a `Local` context names, or an error naming the context that
/// is not local.
fn ctx_local(ctx: &Context, name: &str) -> anyhow::Result<nitsd::launch::DaemonSpec> {
    match ctx {
        Context::Local { data_dir, socket } => {
            Ok(contexts::local_spec(data_dir.as_ref(), socket.as_ref())?)
        }
        Context::Ssh { .. } | Context::Ws { .. } => {
            bail!("context `{name}` is remote; a daemon can only be served locally")
        }
    }
}

fn serve_opts(spec: &nitsd::launch::DaemonSpec, args: &ServeArgs) -> nitsd::serve::ServeOpts {
    nitsd::serve::ServeOpts {
        data_dir: spec.data_dir.clone(),
        socket: spec.socket.clone(),
        idle_exit: args.idle_exit,
        ws: args.ws_listen,
    }
}

/// `nits mcp`: the MCP stdio server, on the context the global flags chose.
async fn mcp(
    selection: Selection,
    config_path: PathBuf,
    start: contexts::StartPolicy,
) -> anyhow::Result<()> {
    init_daemon_logging();
    nits_mcp::serve_stdio(
        nits_mcp::Endpoint {
            selection,
            config_path,
            start,
        },
        nits_mcp::server::AgentIdentity::from_env(),
        BuildInfo {
            name: "nits-mcp".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
    )
    .await
}

/// One line of `daemon status --json`.
#[derive(Debug, Serialize)]
struct Row<'a> {
    context: &'a str,
    target: &'a str,
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    daemon: Option<&'a BuildInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'a str>,
}

/// `review show` output: the snapshot plus the changed files.
#[derive(Debug, Serialize)]
struct Shown<'a> {
    #[serde(flatten)]
    snapshot: &'a nits_protocol::ReviewSnapshot,
    files: &'a [nits_protocol::FileChange],
}

/// The review selected for the requested directory and its owning entities.
#[derive(Debug, Serialize)]
struct DirectoryReview {
    review_id: ReviewId,
    workspace_id: WorkspaceId,
    repo_id: RepoId,
    outcome: DirectoryReviewOutcome,
}

/// Headless JSON output includes the daemon's recorded resolution for this repo.
#[derive(Debug, Serialize)]
struct HeadlessReview<'a> {
    #[serde(flatten)]
    review: &'a DirectoryReview,
    base: &'a nits_protocol::ResolvedRef,
    head: &'a nits_protocol::ResolvedRef,
}

/// Print `v` as JSON when `--json`, else `text`.
fn emit<T: Serialize>(json: bool, v: &T, text: impl FnOnce() -> String) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(v)?);
    } else {
        let t = text();
        if !t.is_empty() {
            println!("{}", t.trim_end_matches('\n'));
        }
    }
    Ok(())
}

fn event_line(e: &Event) -> String {
    let who = match &e.author {
        Author::Human { name, .. } => name.clone(),
        Author::Agent { name, .. } => format!("{name} (agent)"),
        Author::Daemon { .. } => "daemon".into(),
    };
    let what = match &e.body {
        EventBody::WorkspaceCreated { workspace, .. } => {
            format!("workspace created {}", workspace.name)
        }
        EventBody::WorkspaceUpdated { .. } => "workspace updated".into(),
        EventBody::RepoAttached { repo, .. } => format!("repo attached {}", repo.path),
        EventBody::RepoDetached { repo_id, .. } => format!("repo detached {repo_id}"),
        EventBody::ReviewCreated { review } => {
            format!("review created {} {}", review.id, review.title)
        }
        EventBody::ReviewUpdated { review_id, .. } => format!("review updated {review_id}"),
        EventBody::ReviewTargetUpdated { review_id, target } => {
            format!("review target updated {review_id} repo {}", target.repo_id)
        }
        EventBody::ReviewDeleted { review_id } => format!("review deleted {review_id}"),
        EventBody::ReviewTargetsResolved { review_id, .. } => {
            format!("targets resolved {review_id}")
        }
        EventBody::CommentCreated { comment } => format!(
            "comment {} on {}: {}",
            comment.id,
            anchor_text(&comment.anchor),
            comment.body
        ),
        EventBody::CommentEdited { comment_id, .. } => format!("comment edited {comment_id}"),
        EventBody::CommentDeleted { comment_id, .. } => format!("comment deleted {comment_id}"),
        EventBody::CommentReanchored { .. } => "comments re-anchored".into(),
        EventBody::ThreadResolved { thread_id, .. } => format!("thread resolved {thread_id}"),
        EventBody::ThreadUnresolved { thread_id, .. } => format!("thread reopened {thread_id}"),
        EventBody::FileViewed { path, .. } => format!("viewed {path}"),
        EventBody::FileUnviewed { path, .. } => format!("unviewed {path}"),
        EventBody::ReviewRequested { agent, note, .. } => {
            format!("review requested from {agent}: {note}")
        }
        EventBody::SuggestionApplied { comment_id, .. } => {
            format!("suggestion applied {comment_id}")
        }
    };
    format!("#{} {who}: {what}", e.seq)
}

fn anchor_text(a: &Anchor) -> String {
    match a {
        Anchor::Review => "review".into(),
        Anchor::File { path, .. } => path.to_string(),
        Anchor::Lines {
            path, side, lines, ..
        } => {
            format!("{path}:{}-{} ({side:?})", lines.start(), lines.end())
        }
    }
}

#[derive(Debug, clap::Subcommand)]
enum KeysCmd {
    /// Write ~/.config/nits/keys.toml with every action's defaults, plus
    /// keys.schema.json next to it (for editor autocomplete).
    Init {
        /// Overwrite an existing keys.toml.
        #[arg(long)]
        force: bool,
    },
    /// Print the JSON schema for keys.toml (schemars-generated).
    Schema,
    /// Parse a keys file and report collisions (which never reject).
    Check {
        /// Defaults to ~/.config/nits/keys.toml.
        path: Option<PathBuf>,
    },
}

fn keys_cmd(cmd: &KeysCmd) -> anyhow::Result<()> {
    use nits_client_host::keys_file;
    match cmd {
        KeysCmd::Init { force } => {
            let path = keys_file::default_keys_path()
                .ok_or_else(|| anyhow::anyhow!("no home directory"))?;
            if path.exists() && !force {
                anyhow::bail!("{} exists; pass --force to overwrite", path.display());
            }
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&path, keys_file::default_file())?;
            let schema = path.with_file_name("keys.schema.json");
            std::fs::write(&schema, keys_file::schema_json())?;
            println!("wrote {}\nwrote {}", path.display(), schema.display());
            Ok(())
        }
        KeysCmd::Schema => {
            println!("{}", keys_file::schema_json());
            Ok(())
        }
        KeysCmd::Check { path } => {
            let path = match path {
                Some(p) => p.clone(),
                None => keys_file::default_keys_path()
                    .ok_or_else(|| anyhow::anyhow!("no home directory"))?,
            };
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let config = keys_file::parse(&text)?;
            let map = nits_client_core::Keymap::with_config(&config)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let conflicts = map.conflicts();
            if conflicts.is_empty() {
                println!("ok: no collisions");
            } else {
                println!(
                    "ok, with {} collision(s) (later bindings shadow):",
                    conflicts.len()
                );
                for c in conflicts {
                    println!(
                        "  [{}] {} → {}",
                        c.context,
                        c.keys,
                        c.commands
                            .iter()
                            .map(|cmd| nits_client_core::config_name(*cmd))
                            .collect::<Vec<_>>()
                            .join(" vs ")
                    );
                }
            }
            Ok(())
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let matches = <Cli as clap::CommandFactory>::command().get_matches();
    let origin = if matches.value_source("context") == Some(clap::parser::ValueSource::EnvVariable)
    {
        SelectionOrigin::Environment
    } else {
        SelectionOrigin::Flag
    };
    let mut cli = Cli::from_arg_matches(&matches)?;
    if cli.cmd.is_none()
        && cli.path.is_some()
        && let Some(workspace) = cli.workspace
    {
        bail!(
            "--workspace cannot select a workspace when opening a directory path; directory opening selects the repository's existing attachment.\nUse `nits --workspace {workspace} review create --repo <REPO_ID> --base <REF> --head worktree`; find repository IDs with `nits workspace list`."
        );
    }
    let json = cli.json;
    let cfg_path = config_path(&cli)?;
    let mut cfg = nits_config::Config::load(&cfg_path)?;
    if let Some(Cmd::Keys(c)) = &cli.cmd {
        // Purely local: no daemon, no context.
        return keys_cmd(c);
    }
    if matches!(cli.cmd, Some(Cmd::Context(_)))
        && let Some(Cmd::Context(c)) = cli.cmd.take()
    {
        return context_cmd(&mut cfg, &cfg_path, &cli, origin, c, json);
    }
    let selection = resolve_context(&cli, &cfg, origin)?;
    if let Some(Cmd::Mcp) = cli.cmd {
        return mcp(selection, cfg_path, cli.start_policy.into()).await;
    }
    let Selection {
        name, context: ctx, ..
    } = selection;
    if let Some(Cmd::Daemon(c)) = cli.cmd {
        return daemon_cmd(&cfg, name.as_str(), &ctx, c, json, cli.start_policy.into()).await;
    }
    let ops = connect(&cli, &ctx).await?;
    let Some(cmd) = cli.cmd else {
        return open_ui(&cli, &ctx, ops).await;
    };
    let mut ops = ops;
    match cmd {
        Cmd::Context(_) | Cmd::Daemon(_) | Cmd::Keys(_) | Cmd::Mcp => {
            unreachable!("handled above")
        }
        Cmd::Workspace(c) => workspace(&mut ops, c, json).await,
        Cmd::Review(c) => review(&mut ops, c, cli.workspace, json).await,
        Cmd::Comment(c) => comment(&mut ops, c, json).await,
        Cmd::Files { .. } | Cmd::Diff { .. } | Cmd::Show { .. } => content(&ops, cmd, json).await,
        Cmd::Events {
            follow,
            review,
            awaiting,
            since,
        } => events(&ops, follow, review, cli.workspace, awaiting, since, json).await,
    }
}

/// Bare `nits [path]`: with a path, find or create that directory's
/// review (head = working tree); then serve the browser UI in the
/// foreground on a free port and print the URL (deep-linked when a
/// review was resolved). Without a path: the workspace menu.
async fn open_ui(cli: &Cli, ctx: &Context, mut ops: Ops) -> anyhow::Result<()> {
    let directory = if let Some(path) = &cli.path {
        Some(directory_review(&mut ops, ctx, path).await?)
    } else {
        anyhow::ensure!(
            !cli.headless && cli.ui != Ui::Headless,
            "--headless needs a path: it only ensures a review exists"
        );
        None
    };
    let review_id = directory.as_ref().map(|review| review.review_id);
    let ui = if cli.headless { Ui::Headless } else { cli.ui };
    match (ui, directory) {
        (Ui::Headless, Some(review)) => {
            return print_headless_review(&ops, &review, cli.json).await;
        }
        (Ui::Headless, None) => unreachable!("checked above"),
        (Ui::Desktop, _) => {
            // The Tauri app next to this binary (no deep link yet).
            let app = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("nits-desktop")))
                .filter(|p| p.exists())
                .ok_or_else(|| anyhow::anyhow!("nits-desktop not found next to nits"))?;
            let mut child = std::process::Command::new(app);
            child.args(desktop_args(cli));
            child.spawn().context("launching nits-desktop")?;
            return Ok(());
        }
        (Ui::Web, _) => {}
    }
    // The bridge owns a ClientCore and therefore its own daemon connection.
    // Release the command client's SSH proxy before the host dials another.
    drop(ops);
    let endpoint = contexts::DaemonEndpoint::resolve(ctx, cli.start_policy.into())?;
    let (client, author) = principal(cli);
    let host = nits_client_web::web_config(
        endpoint,
        client,
        author,
        nits_client_core::IdSeed(nitsd::ids::fresh_parts().1),
        nits_client_host::KvConfig::Memory,
    );
    let addr = std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, cli.port));
    let server = nits_client_web::serve(addr, host).await?;
    let query = review_id.map_or_else(String::new, |id| format!("?review={id}"));
    println!("\n  nits: http://{}/{query}\n", server.addr());
    tokio::signal::ctrl_c().await?;
    server.stop();
    Ok(())
}

async fn print_headless_review(
    ops: &Ops,
    review: &DirectoryReview,
    json: bool,
) -> anyhow::Result<()> {
    if !json {
        println!("{}", review.review_id);
        return Ok(());
    }
    let snapshot = ops.snapshot(review.review_id).await?;
    let target = snapshot
        .resolved
        .as_ref()
        .and_then(|targets| {
            targets.iter().find(|target| {
                target.repo_id == review.repo_id
                    && matches!(
                        target.head.source,
                        nits_protocol::ResolvedSource::WorkingTree { .. }
                    )
            })
        })
        .context("the daemon has no resolved working-tree target for this directory")?;
    emit(
        json,
        &HeadlessReview {
            review,
            base: &target.base,
            head: &target.head,
        },
        || review.review_id.to_string(),
    )
}

/// Bootstrap paths on the selected daemon, including remote contexts.
async fn directory_review(
    ops: &mut Ops,
    ctx: &Context,
    path: &Path,
) -> anyhow::Result<DirectoryReview> {
    // Relative local paths belong to the invoking shell; remote paths belong
    // to the daemon. No remote path is inspected on the client machine.
    let path = if matches!(ctx, Context::Local { .. }) && path.is_relative() {
        std::env::current_dir()?.join(path)
    } else {
        path.to_path_buf()
    };
    let review = ops
        .ensure_directory_review(path.to_string_lossy().into_owned(), None, None)
        .await?;
    let created = match review.outcome {
        DirectoryReviewOutcome::Created => "created, ",
        DirectoryReviewOutcome::Reused => "",
    };
    eprintln!(
        "review: {} ({created}base: {})",
        review.review_id,
        ref_label(&review.base)
    );
    Ok(DirectoryReview {
        review_id: review.review_id,
        workspace_id: review.workspace_id,
        repo_id: review.repo_id,
        outcome: review.outcome,
    })
}

async fn workspace(ops: &mut Ops, cmd: WorkspaceCmd, json: bool) -> anyhow::Result<()> {
    match cmd {
        WorkspaceCmd::Add { name } => {
            let (id, event) = ops.create_workspace(name).await?;
            emit(json, &event, || id.to_string())
        }
        WorkspaceCmd::List => {
            let ws = ops.workspaces().await?;
            emit(json, &ws, || {
                ws.iter()
                    .map(|w| {
                        let repos: Vec<String> = w
                            .repos
                            .iter()
                            .map(|r| format!("\n  {} {} {}", r.id, r.display_name, r.path))
                            .collect();
                        let names = w
                            .repos
                            .iter()
                            .map(|r| r.display_name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!("{} {} ({}){}", w.id, w.name, names, repos.concat())
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        }
        WorkspaceCmd::Attach {
            workspace_id,
            path,
            name,
        } => {
            let path = path.unwrap_or_else(|| PathBuf::from("."));
            let path =
                std::fs::canonicalize(&path).with_context(|| format!("{}", path.display()))?;
            let display = name.unwrap_or_else(|| {
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
            let (id, event) = ops
                .attach_repo(workspace_id, path.to_string_lossy().into_owned(), display)
                .await?;
            emit(json, &event, || id.to_string())
        }
    }
}

/// Add CLI-specific guidance only when cwd cannot select a workspace;
/// transport failures retain their original connection diagnostics.
async fn locate_review_workspace(ops: &Ops) -> anyhow::Result<nitsd::ops::Located> {
    match ops.locate(Path::new(".")).await {
        Ok(located) => Ok(located),
        Err(nitsd::ops::OpsError::Invalid(reason)) => bail!(
            "{reason}\nSelect a workspace with `nits --workspace <ID> review list` or `nits --workspace <ID> review create --base <REF> --head <REF>`; find IDs with `nits workspace list`."
        ),
        Err(error) => Err(error.into()),
    }
}

/// Repository paths come from the daemon, so remote repositories are labelled
/// correctly even when this process runs from an unrelated local checkout.
fn review_text(review: &Review, workspaces: &[Workspace]) -> String {
    let mut out = format!("{} [{:?}] {}", review.id, review.status, review.title);
    let workspace = workspaces.iter().find(|w| w.id == review.workspace_id);
    if let Some(workspace) = workspace {
        let _ = write!(out, "\n  workspace {} {}", workspace.id, workspace.name);
    } else {
        let _ = write!(out, "\n  workspace {} (unavailable)", review.workspace_id);
    }
    for target in &review.targets {
        let repo = workspace.and_then(|w| w.repos.iter().find(|r| r.id == target.repo_id));
        let _ = write!(out, "\n  repo {}", target.repo_id);
        if let Some(repo) = repo {
            let _ = write!(out, " {} ({})", repo.display_name, repo.path);
        } else {
            out.push_str(" (unavailable)");
        }
        let _ = write!(
            out,
            "\n    {}..{}",
            ref_label(&target.base),
            ref_label(&target.head)
        );
    }
    out
}

async fn review(
    ops: &mut Ops,
    cmd: ReviewCmd,
    workspace: Option<WorkspaceId>,
    json: bool,
) -> anyhow::Result<()> {
    match cmd {
        ReviewCmd::Create {
            base,
            head,
            repo,
            title,
        } => {
            let (workspace, repo_id) = match (workspace, repo) {
                (Some(w), Some(r)) => (w, r),
                (Some(workspace), None) => {
                    let ws = ops.workspaces().await?;
                    let w = ws
                        .iter()
                        .find(|w| w.id == workspace)
                        .ok_or_else(|| anyhow::anyhow!("no workspace {workspace}"))?;
                    let repo_id = match w.repos.as_slice() {
                        [only] => only.id,
                        [] => bail!("workspace has no repos; attach one first"),
                        _ => match ops.locate(Path::new(".")).await {
                            Ok(l) if l.workspace.id == workspace => l.repo.id,
                            _ => bail!("workspace has several repos; pass --repo"),
                        },
                    };
                    (workspace, repo_id)
                }
                (None, repo) => {
                    let l = locate_review_workspace(ops).await?;
                    (l.workspace.id, repo.unwrap_or(l.repo.id))
                }
            };
            let target = ReviewTarget {
                repo_id,
                base: parse_ref(&base)?,
                head: parse_ref(&head)?,
            };
            let title = title.unwrap_or_else(|| format!("{base}..{head}"));
            let (id, event) = ops
                .create_review(workspace, title, NonEmpty::singleton(target))
                .await?;
            emit(json, &event, || id.to_string())
        }
        ReviewCmd::List => {
            let workspace = match workspace {
                Some(w) => w,
                None => locate_review_workspace(ops).await?.workspace.id,
            };
            let reviews = ops.reviews(workspace).await?;
            let workspaces = if json {
                Vec::new()
            } else {
                ops.workspaces().await?
            };
            emit(json, &reviews, || {
                reviews
                    .iter()
                    .map(|r| review_text(r, &workspaces))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        }
        ReviewCmd::Show { review } => {
            let snap = ops.snapshot(review.0).await?;
            let files = ops.files(review.0).await?;
            let workspaces = if json {
                Vec::new()
            } else {
                ops.workspaces().await?
            };
            emit(
                json,
                &Shown {
                    snapshot: &snap,
                    files: &files,
                },
                || {
                    let mut out = review_text(&snap.review, &workspaces);
                    out.push('\n');
                    for f in &files {
                        let _ = writeln!(
                            out,
                            "  {:?} {}",
                            nits_protocol::ChangeKindKind::from(&f.kind),
                            f.path
                        );
                    }
                    let _ = writeln!(
                        out,
                        "  {} threads, {} comments",
                        snap.threads.len(),
                        snap.comments.len()
                    );
                    out
                },
            )
        }
    }
}

async fn content(ops: &Ops, cmd: Cmd, json: bool) -> anyhow::Result<()> {
    match cmd {
        Cmd::Files { review } => {
            let files = ops.files(review.0).await?;
            emit(json, &files, || {
                files
                    .iter()
                    .map(|f| {
                        format!(
                            "{:?} {}",
                            nits_protocol::ChangeKindKind::from(&f.kind),
                            f.path
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        }
        Cmd::Diff {
            review,
            path,
            repo,
            ignore_whitespace,
            context_lines,
        } => {
            let render_opts = RenderOpts {
                ignore_whitespace,
                context_lines,
                ..RenderOpts::default()
            };
            let (file, header, chunks) = ops.diff(review, repo, &path, render_opts).await?;
            emit(json, &(&file, &header, &chunks), || {
                render_text::render(&header, &chunks)
            })
        }
        Cmd::Show {
            review,
            path,
            repo,
            side,
        } => {
            let path = RepoPath::new(path)?;
            let (_, _, header, chunks) = ops.file_at(review, repo, &path, side.into()).await?;
            emit(json, &(&header, &chunks), || {
                render_text::render_blob(&header, &chunks)
            })
        }
        _ => unreachable!("dispatched by main"),
    }
}

async fn comment(ops: &mut Ops, cmd: CommentCmd, json: bool) -> anyhow::Result<()> {
    match cmd {
        CommentCmd::Add(a) => {
            let lines = a
                .line
                .map(LineRange::single)
                .or(a.lines)
                .map(|range| (range.start().get(), Some(range.end().get())));
            let anchor = match a.path {
                None if lines.is_some() => bail!("--line/--lines need --path"),
                None => Anchor::Review,
                Some(p) => {
                    let p = RepoPath::new(p)?;
                    ops.anchor(a.review.0, a.repo, &p, a.side.into(), lines)
                        .await?
                }
            };
            let kind = match a.patch {
                Some(patch) => CommentKind::Suggestion { patch },
                None => CommentKind::Note,
            };
            let (t, event) = ops.new_thread(a.review.0, kind, anchor, a.body).await?;
            emit(json, &event, || t.thread_id.to_string())
        }
        CommentCmd::Reply {
            review,
            thread,
            body,
        } => {
            let (id, event) = ops.reply(review, thread, body).await?;
            emit(json, &event, || id.to_string())
        }
        CommentCmd::Resolve { review, thread } => {
            let event = ops
                .mutate(Mutation::ResolveThread {
                    review_id: review,
                    thread_id: thread,
                })
                .await?;
            emit(json, &event, String::new)
        }
        CommentCmd::List { review } => {
            let snap = ops.snapshot(review.0).await?;
            emit(json, &(&snap.threads, &snap.comments), || {
                let mut out = String::new();
                for t in &snap.threads {
                    let state = match t.resolution {
                        nits_protocol::ThreadResolution::Open => "open",
                        nits_protocol::ThreadResolution::Resolved { .. } => "resolved",
                    };
                    let _ = writeln!(out, "thread {} [{state}]", t.id);
                    for id in std::iter::once(&t.root).chain(t.replies.iter()) {
                        if let Some(c) = snap.comments.iter().find(|c| c.id == *id) {
                            let who = match &c.author {
                                Author::Human { name, .. } | Author::Agent { name, .. } => {
                                    name.as_str()
                                }
                                Author::Daemon { .. } => "daemon",
                            };
                            let _ = writeln!(
                                out,
                                "  {} {who} @ {}: {}",
                                c.id,
                                anchor_text(&c.anchor),
                                c.body
                            );
                        }
                    }
                }
                out
            })
        }
    }
}

async fn events(
    ops: &Ops,
    follow: bool,
    review: Option<ReviewId>,
    workspace: Option<WorkspaceId>,
    awaiting: Option<String>,
    since: Option<u64>,
    json: bool,
) -> anyhow::Result<()> {
    {
        let scope = match (review, workspace, awaiting) {
            (Some(review_id), _, _) => SubscribeScope::Review { review_id },
            (None, Some(workspace_id), _) => SubscribeScope::Workspace { workspace_id },
            (None, None, Some(agent)) => SubscribeScope::AwaitingAgent { agent },
            (None, None, None) => SubscribeScope::All,
        };
        let mut since = since.map_or(Since::Now, |n| Since::After { seq: Seq::new(n) });
        loop {
            let timeout = if follow {
                Duration::from_hours(1)
            } else {
                Duration::ZERO
            };
            let polled = ops.poll_events(scope.clone(), since, timeout, 1000).await?;
            for e in &polled.events {
                emit(json, e, || event_line(e))?;
            }
            if !follow {
                break;
            }
            since = Since::After {
                seq: polled.last_seq,
            };
        }
        Ok(())
    }
}

fn context_cmd(
    cfg: &mut nits_config::Config,
    path: &Path,
    cli: &Cli,
    origin: SelectionOrigin,
    cmd: ContextCmd,
    json: bool,
) -> anyhow::Result<()> {
    match cmd {
        ContextCmd::List => emit(json, cfg, || {
            let mut rows: Vec<String> = cfg
                .contexts
                .iter()
                .map(|(n, c)| format!("{n}\t{}", c.describe()))
                .collect();
            if !cfg.contexts.contains_key(nits_config::DEFAULT_CONTEXT) {
                rows.insert(
                    0,
                    format!("{}\tlocal (implicit)", nits_config::DEFAULT_CONTEXT),
                );
            }
            rows.join("\n")
        }),
        ContextCmd::Show => {
            let selection = resolve_context(cli, cfg, origin)?;
            emit(json, &selection, || {
                format!(
                    "{}\t{}\t{:?}",
                    selection.name,
                    selection.context.describe(),
                    selection.origin
                )
            })
        }
        ContextCmd::Use { name } => {
            let selection = cfg.selection(Some((&name, SelectionOrigin::Persisted)))?;
            cfg.current_context = Some(name);
            cfg.save(path)?;
            emit(json, &selection, || {
                format!(
                    "default context: {}\t{}",
                    selection.name,
                    selection.context.describe()
                )
            })
        }
        ContextCmd::AddLocal {
            name,
            data_dir,
            socket,
        } => add(cfg, path, &name, &Context::Local { data_dir, socket }, json),
        ContextCmd::AddSsh {
            name,
            host,
            bin,
            args,
        } => add(
            cfg,
            path,
            &name,
            &Context::Ssh {
                host,
                bin: bin.map_or(
                    nits_config::RemoteBin::Default,
                    nits_config::RemoteBin::Nits,
                ),
                args,
                ssh: None,
            },
            json,
        ),
        ContextCmd::AddWs { name, url } => add(cfg, path, &name, &Context::Ws { url }, json),
        ContextCmd::Remove { name } => {
            let removed = cfg.remove(&name)?;
            cfg.save(path)?;
            emit(json, &removed, || format!("removed {name}"))
        }
    }
}

/// Add (or replace) a context.
fn add(
    cfg: &mut nits_config::Config,
    path: &Path,
    name: &str,
    ctx: &Context,
    json: bool,
) -> anyhow::Result<()> {
    cfg.contexts.insert(name.to_string(), ctx.clone());
    cfg.save(path)?;
    emit(json, &(&name, &ctx), || {
        format!("added {name}\t{}", ctx.describe())
    })
}

async fn daemon_cmd(
    cfg: &nits_config::Config,
    name: &str,
    ctx: &Context,
    cmd: DaemonCmd,
    json: bool,
    start: contexts::StartPolicy,
) -> anyhow::Result<()> {
    match cmd {
        // Being the daemon, and reaching it over a pipe. Both need a place
        // to listen rather than someone to talk to, so only a local context
        // makes sense: a remote one is reached *through* `daemon stdio`.
        DaemonCmd::Serve(args) => {
            init_daemon_logging();
            nitsd::serve::serve(serve_opts(&ctx_local(ctx, name)?, &args)).await
        }
        DaemonCmd::Stdio(args) => {
            init_daemon_logging();
            let opts = serve_opts(&ctx_local(ctx, name)?, &args);
            match nitsd::serve::stdio(opts, start == contexts::StartPolicy::StartIfNeeded).await? {
                nitsd::serve::StdioOutcome::Proxied => Ok(()),
                // Not an error: the caller asked whether one was running.
                nitsd::serve::StdioOutcome::NotRunning => std::process::exit(3),
            }
        }
        DaemonCmd::Status { all } => {
            let mut targets: Vec<(String, Context)> = if all {
                let mut v: Vec<(String, Context)> = cfg
                    .contexts
                    .iter()
                    .map(|(n, c)| (n.clone(), c.clone()))
                    .collect();
                if !cfg.contexts.contains_key(nits_config::DEFAULT_CONTEXT) {
                    v.insert(0, cfg.resolve(Some(nits_config::DEFAULT_CONTEXT))?);
                }
                v
            } else {
                vec![(name.to_string(), ctx.clone())]
            };
            let mut rows = Vec::new();
            for (n, c) in targets.drain(..) {
                rows.push((n, c.describe(), contexts::status(&c).await));
            }
            let json_rows: Vec<Row<'_>> = rows
                .iter()
                .map(|(n, t, s)| Row {
                    context: n,
                    target: t,
                    status: match s {
                        Status::Running { .. } => "running",
                        Status::Stopped => "stopped",
                        Status::Unreachable { .. } => "unreachable",
                    },
                    daemon: match s {
                        Status::Running { daemon } => Some(daemon),
                        _ => None,
                    },
                    reason: match s {
                        Status::Unreachable { reason } => Some(reason),
                        _ => None,
                    },
                })
                .collect();
            emit(json, &json_rows, || {
                rows.iter()
                    .map(|(n, t, s)| {
                        let st = match s {
                            Status::Running { daemon } => {
                                format!("running ({} {})", daemon.name, daemon.version)
                            }
                            Status::Stopped => "stopped".into(),
                            Status::Unreachable { reason } => format!("unreachable: {reason}"),
                        };
                        format!("{n}\t{t}\t{st}")
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
        }
        DaemonCmd::Start => {
            let started = contexts::start(ctx).await?;
            emit(json, &started, || {
                if started {
                    "started"
                } else {
                    "already running"
                }
                .into()
            })
        }
        DaemonCmd::Stop => {
            let stopped = contexts::stop(ctx).await?;
            emit(json, &stopped, || {
                if stopped { "stopping" } else { "not running" }.into()
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// clap's own consistency check over the whole command tree.
    #[test]
    fn the_command_definition_is_valid() {
        <Cli as clap::CommandFactory>::command().debug_assert();
    }

    #[test]
    fn line_ranges_are_inclusive_and_validated_before_connecting() {
        for value in ["1-1", "1:1"] {
            assert_eq!(
                parse_lines(value).unwrap(),
                LineRange::single(LineNo::FIRST)
            );
        }
        for value in ["3-5", "3:5"] {
            let range = parse_lines(value).unwrap();
            assert_eq!(range.start().get(), 3);
            assert_eq!(range.end().get(), 5);
            assert_eq!(range.len(), 3);
        }
        assert!(parse_line("0").is_err());
    }

    #[test]
    fn start_policy_is_parsed_as_an_exhaustive_choice() {
        let default = Cli::try_parse_from(["nits", "workspace", "list"]).unwrap();
        assert_eq!(default.start_policy, StartPolicyArg::StartIfNeeded);

        let required = Cli::try_parse_from([
            "nits",
            "--start-policy",
            "require-running",
            "workspace",
            "list",
        ])
        .unwrap();
        assert_eq!(required.start_policy, StartPolicyArg::RequireRunning);
    }

    #[test]
    fn desktop_arguments_preserve_an_ad_hoc_endpoint_and_policy() {
        let cli = Cli::try_parse_from([
            "nits",
            "--ws",
            "ws://review.example:7677",
            "--start-policy",
            "require-running",
            "--ui",
            "desktop",
        ])
        .unwrap();
        assert_eq!(
            desktop_args(&cli),
            [
                "--start-policy",
                "require-running",
                "--ws",
                "ws://review.example:7677"
            ]
            .map(std::ffi::OsString::from)
        );
    }
}
