# Repository targets and older duplicate reviews

A new review must include at least one repository, with exactly one base/head
pair for each repository. Identical repeated targets are rejected too. To compare
the same repository against different bases or heads, create separate reviews.

The creation form offers repositories that are not already selected in another
row. **+ target** and its configured keyboard command stop adding rows once all
workspace repositories are included. Removing a row makes its repository
available again. A rejected submission keeps its title and target inputs visible.
MCP resolves omitted repository IDs from its working directory before checking
uniqueness; omitting an ID twice does not select two different repositories.

## Repair an older review with duplicate repositories

Older versions could persist a review containing the same repository twice. The
original review and its events, requested targets, checkpoints and comments remain
readable. There is no automatic deduplication: two entries may represent different
comparisons, and choosing one would lose intent. Updating one target is not a
repair because older duplicate entries remain in that review.

Use the following archive-and-replacement workflow in the same daemon context:

1. Run `nits -c <CONTEXT> --json review show <ORIGINAL>` or call MCP
   `get_review` with the original `review_id`. Inspect `review.targets`
   (the requested refs), resolved targets, review requests/checkpoints and comments.
   Record the original ID and workspace. Choose exactly one base/head pair per
   repository; conflicting entries require an explicit choice.
2. Run `nits -c <CONTEXT> review archive <ORIGINAL>`, or call MCP
   `update_review` with the original ID, its existing title, and
   `"status": "Archived"`. Archiving preserves targets and discussion, and does
   not try to select or resolve a duplicate. Keep the original archived if creating
   its replacement fails; correct the replacement's refs and retry.
3. Create the replacement in that workspace using the UI or MCP `create_review`.
   Its `targets` contains only the pairs chosen in step 1. For a single repository,
   `nits review create --help` describes the equivalent CLI operation. Inspect the
   resulting files and diff before treating the replacement as the active review.
4. Add a **review-wide** comment to each record using `add_comment`: supply only
   `review_id` and `body` for the anchor (omit `path`, `start_line` and `end_line`).
   On the original, write "Replaced by …" with the new review's link. On the new
   review, write "Replaces …; earlier discussion remains there" with the old link.
   Review-wide comments work on archived records and do not need an unambiguous
   file. Canonical links have the form
   `nits://context/<context-name>/review/<review-id>`; use the actual named context
   and IDs for both records. The equivalent CLI comments are:

   ```sh
   nits -c <CONTEXT> comment add <ORIGINAL> --intent informational --body 'Replaced by nits://context/<CONTEXT>/review/<NEW>'
   nits -c <CONTEXT> comment add <NEW> --intent informational --body 'Replaces nits://context/<CONTEXT>/review/<ORIGINAL>; earlier discussion remains there'
   ```

   Replace every placeholder with the selected context and actual IDs.

The replacement resolves its refs when it is created. Branches or `worktree` may
have changed since the original review; inspect their current content and choose
explicit commits where appropriate. This workflow does not copy a historical
working-tree snapshot or migrate comments to new anchors. Do not delete the
original record or edit event JSON: its history and bidirectional links preserve
the discussion and the reason for the chosen replacement.
