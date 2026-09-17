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

For scripts, `nits --json . --headless` creates or reuses the directory's
working-tree review and prints one JSON object with `review_id`, `workspace_id`,
`repo_id`, `outcome` (`"Created"` or `"Reused"`), and `base`/`head`. The latter are
the daemon's recorded resolved refs, each containing a `tree` OID and `source`
(commit OID or working-tree details). `--ui headless` is equivalent. Without
`--json`, stdout remains just the review ID; status messages go to stderr.

## Licence

MIT.
