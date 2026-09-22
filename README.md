# Nits

A daemon-backed code review tool. Nits are anchored to content (blobs), not to diffs or line numbers — so they survive rebases, amends and force-pushes.

- One daemon owns each selected data store, with workspaces, reviews and comments in an append-only event log (redb). `nits daemon serve` starts on demand; separate local or remote contexts can use separate stores.
- The browser UI, optional Tauri desktop app, CLI and MCP agents share the same Core through local sockets, SSH or WebSocket connections. A TUI is planned.
- A workspace groups multiple git repos; a review spans any base vs any head across them, with commit stepping.
- GitHub-style diff review plus a file explorer over any ref; inline, file-level and review-level comments; human and agent authorship recorded.
- The UI is keyboard-first, with repository-aware content caches, retained drafts and explicit recovery when a connection or installed build changes.

## Install

One binary, `nits`: the CLI, the daemon (`nits daemon serve`) and the MCP
server (`nits mcp`) are all in it.

This README describes **current source on `main`**. The published `nits-v0.1.0`
release predates several features below, including `nits skill`. Package-manager
installs follow published releases; merging a change to `main` does not publish
a new binary. To use the current source on Linux or macOS, install Rust and Git,
then build from a recorded checkout:

```sh
git clone https://github.com/JonoPrest/nits.git nits-source
cd nits-source
git switch --detach "$(git rev-parse origin/main)"
git rev-parse HEAD                  # save this commit to reproduce the build
cargo install --locked --path crates/nits-cli --force
```

The checked-in browser bundle is included. Rebuilding the UI itself additionally
requires Node and pnpm; see [contributor guidance](AGENTS.md). The optional desktop
app is a separate build, not part of `cargo install nits`.

Published installation options:

```console
$ brew install jonoprest/nits/nits          # macOS, Linuxbrew
$ cargo install nits --locked               # supported Unix platform + Rust
$ yay -S nits-bin                           # Arch
```

Debian/Ubuntu and Fedora/RHEL repositories, and tarballs for
`{x86_64,aarch64}-{apple-darwin,unknown-linux-gnu,unknown-linux-musl}`, are at
<https://jonoprest.github.io/nits/> and on the [releases page][releases].
There is no Windows build yet; native daemon lifecycle support requires Unix.

Installing a new executable does not replace an already-running daemon. Inspect
`nits daemon upgrade-status --json`, then explicitly activate the selected install
with `nits daemon upgrade --json` when needed. Equal package versions with different
digests, as in repeated development builds, do not establish automatic upgrade
order. Older builds predating coordinated upgrades require the manual stop/start
bootstrap described in [daemon upgrades](docs/DAEMON-UPGRADES.md).

## Start a review

From a Git checkout, run:

```sh
nits .
```

Open the printed HTTP URL and leave the command running. It attaches the checkout
if needed and creates or reuses its working-tree review. Running `nits` without a
path opens the workspace menu. For scripts, `nits . --headless` prints the review ID;
`nits --json . --headless` also returns workspace/repository IDs and resolved refs.

The [runnable quickstart](docs/QUICKSTART.md) starts from a fresh repository, carries
those IDs into diff/comment commands, and sets up a workspace with two repositories.
The [CLI reference](crates/nits-cli/README.md) covers discovery, filtering and maintenance.

## Review with an agent

Start a review with **Claude Code** after installing Nits:

> Run `nits skill`, read its instructions, and use Nits to review the changes I requested.

`nits skill` prints the installed version's self-contained review guide, including
CLI and MCP interaction and revision workflows. It works outside a checkout,
without a daemon or network connection, and does not install anything. Claude
can use Nits MCP tools if they are already available; otherwise it can use the
CLI immediately. MCP is optional: `claude mcp add nits -- nits mcp` configures it
when you want that integration.

For an optional persistent Claude Code skill, export the same complete guide:

```sh
mkdir -p ~/.claude/skills/nits-review
nits skill > ~/.claude/skills/nits-review/SKILL.md
```

Then invoke `/nits-review` with your review request in Claude Code. For one
project, use `.claude/skills/nits-review/SKILL.md` instead. The exported file
contains its supporting references, so no repository checkout or extra files are
required. Run the export again after upgrading Nits. See the
[Claude Code skill documentation](https://code.claude.com/docs/en/skills) for
personal/project skill discovery.

The same `nits skill` instructions work with other shell-capable agents. Codex
users can optionally export to `${CODEX_HOME:-$HOME/.codex}/skills/nits-review/SKILL.md`
and invoke `$nits-review`; create that directory first. Agent-specific setup is
separate from the portable instructions. The [canonical review skill](skills/nits-review/SKILL.md)
and its references are maintained once and bundled into every Nits binary.

An agent should inspect `get_session_identity` and use accurate display/routing
name and model with `set_session_identity` when MCP is available. See
[session identity and targeted requests](docs/MCP-IDENTITY.md).

Cutting a release: [`docs/RELEASING.md`](docs/RELEASING.md).

[releases]: https://github.com/JonoPrest/nits/releases

## Status

Current source includes the browser review UI, optional Tauri host, CLI and MCP
server. Reviews can span repositories and retain discussions across target moves,
with explicit outdated anchors, deferred findings, suggestion previews and durable
apply receipts. Browse supports repository-specific revisions; diffs distinguish
file modes, symlinks, submodules and line endings. Large views use bounded delivery
frames and source text remains copyable.

Remote contexts, explicit fetch/ref selection, captured review requests/checkpoints,
cross-workspace discovery and filtered joined conversations support ongoing review
loops. Installed-build activation coordinates daemon ownership and keeps supported
MCP sessions initialized; uncertain mutations are reconciled rather than blindly
replayed. The tool catalog and schemas are derived from the installed implementation.

The TUI, standalone browser-Wasm/IndexedDB host, OS-level deep-link registration and
GitHub PR integration remain future work. The milestone plan includes design targets
and test goals; it is not a claim that every planned client or benchmark is delivered.

## Read

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — the design and all resolved decisions.
- [`docs/PLAN.md`](docs/PLAN.md) — milestone design and test strategy, with current implementation boundaries.
- [`docs/DAEMON-UPGRADES.md`](docs/DAEMON-UPGRADES.md) — installed-build activation, connected-client recovery and legacy bootstrap.
- [`docs/REVIEW-TARGETS.md`](docs/REVIEW-TARGETS.md) — repository target rules and preserving older duplicate reviews during repair.
- [`AGENTS.md`](AGENTS.md) — principles and conventions for anyone (human or agent) contributing.

## Naming

Project **Nits** · one binary `nits` (daemon `nits daemon serve`, MCP server `nits mcp`) · crates `nits-*`, with `nitsd` and `nits-mcp` as libraries it links · data dir `~/.local/share/nits/`.
