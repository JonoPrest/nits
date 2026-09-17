# nits

A daemon-backed code review tool. Nits are anchored to content (blobs), not to
diffs or line numbers — so they survive rebases, amends and force-pushes.

```console
$ cargo install nits
$ nits workspace add .
$ nits review create --base main --head HEAD
$ nits diff
```

- A workspace groups multiple git repos; a review spans any base vs any head
  across them, with commit stepping.
- Inline, file-level and review-level comments, with human and agent authorship
  recorded.
- Everything is served by one daemon per machine, reachable locally or over
  SSH. The daemon is this same binary — `nits daemon serve`, started on demand
  — so there is nothing else to install and no way for the two to be different
  versions. Agents attach through `nits mcp`.

Other ways to install — Homebrew, `apt`, `dnf`, the AUR — and the full
documentation are at <https://github.com/JonoPrest/nits>.

Select a workspace with `--workspace <ID>` anywhere in the command, including
before `review list`, `review create`, or `events`. Without it, review commands
infer the workspace from the current directory. `nits workspace list` lists IDs,
repository names and paths; `review list` and `review show` also identify each
repository and its base/head refs, including when using a remote context.
The `nits PATH` shortcut (including `--headless`) selects its workspace from the
repository's existing attachment and rejects `--workspace`. For an explicit
workspace, use `nits --workspace <ID> review create --repo <REPO_ID> --base <REF>
--head worktree`; `nits workspace list` shows the repository IDs.

```console
$ nits -c build-box --workspace <ID> review list
$ nits --daemon-url ws://reviews.example:7677 workspace list
$ nits comment list --review <REVIEW>
$ nits comment add <REVIEW> --path src/main.rs --lines 12-15 --body 'Check these lines'
```

`--daemon-url` chooses an ad-hoc WebSocket daemon; `--ws` is a deprecated alias
and `NITS_WS_URL` remains supported. `comment add`, `comment list`, `review show`,
and `files` accept either positional `<REVIEW>` or `--review <REVIEW>`, but not
both. Commands with a second positional (a path or thread) take `<REVIEW>` first. Line ranges accept
`START-END` and `START:END`; both are inclusive and require positive, ordered
line numbers. `--json` retains the protocol values for scripting.

For scripts, `nits --json . --headless` creates or reuses the directory's
working-tree review and prints one JSON object with `review_id`, `workspace_id`,
`repo_id`, `outcome` (`"Created"` or `"Reused"`), and `base`/`head`. The latter are
the daemon's recorded resolved refs, each containing a `tree` OID and `source`
(commit OID or working-tree details). `--ui headless` is equivalent. Without
`--json`, stdout remains just the review ID; status messages go to stderr.

## Licence

MIT.
