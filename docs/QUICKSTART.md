# A first review

Use a current-source Nits installation as described in the [README](../README.md#install).
These examples require Git and a POSIX shell. They use a separate data directory;
run them in a fresh shell so the exported demo settings do not affect later work.

## Create a disposable checkout

```sh
demo_root=$(mktemp -d)
unset NITS_SOCKET NITS_WS_URL NITS_CONTEXT NITS_BIN
export NITS_DATA_DIR="$demo_root/nits-data"
export NITS_CONFIG="$demo_root/config.toml"
touch "$NITS_CONFIG"

git init -b main "$demo_root/alpha"
git -C "$demo_root/alpha" config user.name 'Ada Example'
git -C "$demo_root/alpha" config user.email 'ada@example.com'
printf 'hello\n' > "$demo_root/alpha/hello.txt"
git -C "$demo_root/alpha" add hello.txt
git -C "$demo_root/alpha" commit -m 'Initial greeting'
printf 'hello, review\n' > "$demo_root/alpha/hello.txt"
```

## Open the directory review

The caller can be outside the checkout. This attaches Alpha if needed and creates
or reuses its working-tree review, with a detected base (here `main`):

```sh
review_id=$(nits "$demo_root/alpha" --headless)
nits review show "$review_id"
nits files "$review_id"
nits diff "$review_id" hello.txt
```

The headless command prints only the review ID to stdout; status goes to stderr.
For all bootstrap IDs and resolved refs, use:

```sh
nits --json "$demo_root/alpha" --headless
```

Its object has `review_id`, `workspace_id`, `repo_id`, `outcome`, `base` and `head`.
Those IDs have different roles; do not substitute one for another.

To open the same review in a browser:

```sh
nits "$demo_root/alpha"
```

Open the printed HTTP URL. Keep this command running while using the UI, then
press Ctrl-C to stop the browser bridge. The daemon and saved review remain.
Running `nits` with no path opens the workspace menu. `--ui desktop` requires a
separately built `nits-desktop` beside the CLI; the browser is the default.

## Discuss and discover

After stopping the browser bridge, continue in the same shell:

```sh
thread_id=$(nits comment add "$review_id" --path hello.txt --line 1 \
  --body 'Should the greeting include the audience?')
nits comment reply "$review_id" "$thread_id" --body 'This is the review example.'
nits comment list "$review_id" --open
nits --json comment list "$review_id" --thread "$thread_id"
nits review request "$review_id" reviewer-a --note 'Please inspect the greeting.'
nits review list --awaiting reviewer-a
```

Line numbers are inclusive source coordinates on the head by default. The first
comment's ID is also its thread ID; replies get separate comment IDs. Joined
comment-list JSON includes ordered comments, status counts and a coherent `seq`.
`--since SEQ` selects complete threads with later activity; combine filters to
query current state, not to obtain a removal feed. A resolved thread disappears
from `--open --since`; use unfiltered activity or events to track that transition.

A request records the selected targets. It does not select a commit mentioned in
its note, imply approval or answer an earlier request. A warning says when its
captured targets equal the latest checkpoint; historical uncertainty is explicit.

## Attach two repositories to one workspace

Create a second checkout and an explicit workspace. These commands work from
outside both repositories:

```sh
git init -b main "$demo_root/beta"
git -C "$demo_root/beta" config user.name 'Ada Example'
git -C "$demo_root/beta" config user.email 'ada@example.com'
printf 'beta\n' > "$demo_root/beta/hello.txt"
git -C "$demo_root/beta" add hello.txt
git -C "$demo_root/beta" commit -m 'Initial beta'
printf 'beta, review\n' > "$demo_root/beta/hello.txt"

workspace_id=$(nits workspace add 'Two repositories')
alpha_id=$(nits workspace attach "$workspace_id" "$demo_root/alpha" --name Alpha)
beta_id=$(nits workspace attach "$workspace_id" "$demo_root/beta" --name Beta)
nits workspace list
alpha_review=$(nits --workspace "$workspace_id" review create --repo "$alpha_id" \
  --base main --head worktree --title 'Alpha greeting')
beta_review=$(nits --workspace "$workspace_id" review create --repo "$beta_id" \
  --base main --head worktree --title 'Beta greeting')
nits --workspace "$workspace_id" review list
nits diff "$beta_review" hello.txt --repo "$beta_id"
```

The inventory reports both names, paths and IDs. The UI labels repository roots,
including identical paths such as `hello.txt`; its creation dialog lets you select
multiple targets and its Browse controls select the repository explicitly.
One checkout may belong to multiple workspaces, as Alpha does here. It may appear
only once within a workspace; repeated attachment returns its existing identity.
Linked Git worktrees are separate checkouts.

CLI `review create` selects **one repository**. To create one review spanning both,
use the UI creation dialog or MCP `create_review` with a nonempty `targets` array:

```json
{
  "workspace_id": "WORKSPACE_ID",
  "title": "Both greetings",
  "targets": [
    {"repo_id": "ALPHA_REPO_ID", "base": {"type": "Branch", "name": "main"}, "head": {"type": "WorkingTree"}},
    {"repo_id": "BETA_REPO_ID", "base": {"type": "Branch", "name": "main"}, "head": {"type": "WorkingTree"}}
  ]
}
```

Replace the symbolic IDs with the values returned above. MCP and the CLI must
select the same daemon context; IDs are not portable across independent stores.

## Pushed fixes on a remote daemon

For a configured SSH context, the paths and Git objects belong to the remote
daemon. After pushing a branch, explicitly fetch its named remote, then select the
remote-tracking ref if the review still follows local `HEAD`:

```sh
nits -c review-box review fetch REVIEW_ID --repo REPO_ID --remote origin
nits -c review-box review set-head REVIEW_ID origin/feature --repo REPO_ID
nits -c review-box review request REVIEW_ID reviewer-a --note 'Please inspect the pushed fix.'
```

This block uses your context and IDs, not the disposable local fixture. Fetch
updates remote-tracking heads, not the checkout, index, local branches, tags or
`FETCH_HEAD`. It refreshes existing selected refs; it does not silently change a
review from local `HEAD` to `origin/feature`. A receipt can report a successful
fetch with a separate target-resolution failure. Ref selection accepts available
short/full commit IDs, remote refs, tags and single-commit Git expressions, with
`branch:NAME` and `tag:NAME` to disambiguate. It never fetches implicitly.

## Finish the example

Stop the demo daemon before discarding its temporary directory:

```sh
nits daemon stop
```

Keep `$demo_root` to revisit the saved reviews, or remove that disposable directory
when finished. Close the demo shell to leave its configuration behind. On a real
checkout, stopping the browser or daemon does not delete reviews or source files.
