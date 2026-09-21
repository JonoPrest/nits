# Nits

A daemon-backed code review tool. Nits are anchored to content (blobs), not to diffs or line numbers — so they survive rebases, amends and force-pushes.

- One always-running daemon per machine owns workspaces, reviews and comments in an append-only event log (redb). It is the same binary — `nits daemon serve` — started on demand, so client and daemon can never be different versions.
- Clients — Tauri desktop (first), browser, TUI, CLI (`nits`), agents via MCP — attach over the same JSON protocol, locally or through an SSH tunnel.
- A workspace groups multiple git repos; a review spans any base vs any head across them, with commit stepping.
- GitHub-style diff review plus a file explorer over any ref; inline, file-level and review-level comments; human and agent authorship recorded.
- Clients cache everything by OID (memory + disk), apply mutations optimistically, and are keyboard-first.

## Install

One binary, `nits`: the CLI, the daemon (`nits daemon serve`) and the MCP
server (`nits mcp`) are all in it.

```console
$ brew install jonoprest/nits/nits          # macOS, Linuxbrew
$ cargo install nits                        # any platform with a Rust toolchain
$ yay -S nits-bin                           # Arch
```

Debian/Ubuntu and Fedora/RHEL repositories, and tarballs for
`{x86_64,aarch64}-{apple-darwin,unknown-linux-gnu,unknown-linux-musl}`, are at
<https://jonoprest.github.io/nits/> and on the [releases page][releases].
There is no Windows build yet — the daemon's transport is unix-socket only.

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

Milestone 1.1–1.2 done: Cargo workspace, CI, `nits-protocol` (all wire types + JSON fixtures), `nits-test-support` (real-git `RepoBuilder`). 1.3 done: redb event store with schema versioning. 1.4 done: git engine (gix + git CLI). **Milestone 1 complete** (protocol, store, git engine, render model, reviews, comments + anchoring, `Core` façade). Milestone 2.1–2.2 done: `nitsd` (length-prefixed JSON frames, version handshake, unix-socket/stdio server, subscriptions with gap replay, streamed `open_review`, async `nitsd::client`). 2.3 done: debounced file watcher (`TreeDelta` + re-resolve of working-tree reviews). 2.4 (WebSocket, `nits daemon serve --ws-listen 127.0.0.1:7677`) done. 2.5 done: `nits-mcp` stdio server (20 tools, agent provenance from `initialize` and session identity tools, `subscribe_events` long-poll). 2.6 done: `nits` CLI (`workspace add|list|attach`, `review create|list|show`, `files`, `diff`, `show`, `comment add|reply|resolve|list`, `events [--follow]`, `--json`, `--agent`). 2.7 done: lifecycle (`--data-dir`/`--socket`/`daemon stdio`, ctrl-c shutdown, stale-socket reclaim; kill -9 mid-burst test reopens consistent). **Milestone 2 complete.** Contexts + daemon lifecycle: `nits context add-ssh box user@host` / `nits -c box …` / `nits context use box` (persisted default for new processes; explicit flags override it; workspace/repo default from cwd), `nits daemon status|start|stop`, `nits daemon stdio` proxies to (and auto-starts) the one daemon per machine. 3.0 done: benchmark triggers (`cargo bench`, `docs/BENCHMARKS.md`; snapshot-after-edit and comment-burst triggers tripped, fixes pending). 3.1 done: `nits-client-core` sans-I/O state machine (`ClientCore::handle(Input) -> Result<Vec<Effect>, CoreError>`, connection `Disconnected | Connecting | Subscribed { last_seq }` with reconnect via `Since::After`, draft open/submit/discard with deferred refresh, proptest over random input sequences, wasm check in CI). Single binary: the daemon and the MCP server became `nits daemon serve` and `nits mcp`, `nitsd` and `nits-mcp` are libraries, and one install is the whole thing. Next: 3.2 cache — see `docs/HANDOVER.md`.

## Read

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) — the design and all resolved decisions.
- [`docs/PLAN.md`](docs/PLAN.md) — four milestones with per-task test strategy. Start at **Milestone 1.1**.
- [`docs/REVIEW-TARGETS.md`](docs/REVIEW-TARGETS.md) — repository target rules and preserving older duplicate reviews during repair.
- [`AGENTS.md`](AGENTS.md) — principles and conventions for anyone (human or agent) contributing.

## Naming

Project **Nits** · one binary `nits` (daemon `nits daemon serve`, MCP server `nits mcp`) · crates `nits-*`, with `nitsd` and `nits-mcp` as libraries it links · data dir `~/.local/share/nits/`.
