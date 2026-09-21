# GitHub integration artboards

Three static design concepts accompany the [issue #49 proposal](../../docs/GITHUB-PR-INTEGRATION.md):

- [Inline discussions](inline.png): imported GitHub threads and selected local nits.
- [Publish preview](publish.png): exact outgoing comments, account and blocked placement.
- [Connection](connect.png): proposed later GitHub App device flow for an SSH daemon.

These are proposed UI states with invented sample content, not a functioning
integration. The initial auth release uses an existing `gh` account. Source:
[artboards.html](artboards.html), using the repository's dark UI colors and layout.
Open it with `#inline`, `#publish` or `#connect` to select a view. No network,
runtime dependencies, credentials or real device code are involved.

Render each at 1440 × 1040 with a Chromium-based browser. For example, from the
repository root (set `NITS_MOCKUP_CHROME` to your browser executable):

```sh
"$NITS_MOCKUP_CHROME" --headless --disable-gpu --hide-scrollbars \
  --no-pdf-header-footer --window-size=1440,1040 --force-device-scale-factor=1 \
  --screenshot="$PWD/design/github-pr/inline.png" \
  "file://$PWD/design/github-pr/artboards.html#inline"
```

Repeat for `publish` and `connect`, changing both the output name and fragment.
The images deliberately show proposed keyboard hints; production hints must come
from `nits-client-core`'s keymap. The source is presentation-only: button shapes do
not dispatch product actions. Do not copy it into the ReScript UI as implementation.
