# MCP session identity

After MCP initialization, call `get_session_identity` with `{}`. Both identity
tools return an `author` object in their structured content, for example:

```json
{
  "author": {
    "type": "Agent",
    "name": "example-mcp-client",
    "model": "unknown",
    "session_id": "session-a",
    "invoked_by": { "name": "ada", "machine": "devbox" },
    "via": "Mcp"
  }
}
```

The initial name comes from `initialize.clientInfo.name`; the initial model
comes from `NITS_AGENT_MODEL` (default `unknown`). `NITS_SESSION_ID` can supply
the session ID; otherwise Nits generates one. The invoking human comes from
the launch environment and may be null when unknown.

Before posting, call `set_session_identity` with the name and model actually
used by this agent:

```json
{ "name": "reviewer-a", "model": "example-model" }
```

Both fields are required. Read the current identity first to keep either value
unchanged. Values must be nonempty, with no surrounding whitespace or control
characters. Other fields, including `session_id`, `invoked_by`, and `via`, are
rejected. The returned author is used for subsequent comments and other events;
existing comments and events keep their original author. Manual attribution in
comment text is still allowed.

Each MCP server controls only its own session. Identity updates preserve its
session ID, invoking human, and `Agent`/`Mcp` provenance. They negotiate a fresh
daemon connection without restarting MCP. An update becomes active only after
that handshake succeeds; if it fails, read the unchanged identity and retry
after restoring daemon connectivity. Identity reads remain available while the
daemon is disconnected. Automatic reconnects retain the last successful update.
These tools append no event and do not persist settings across MCP restarts.

## Targeted review requests

Share the returned `author.name` with the requesting agent or human. Choose a
distinct name for each collaborating agent and keep it stable. The exact same
string is used in `request_review`:

```json
{
  "review_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
  "agent": "reviewer-a",
  "note": "Please review this change"
}
```

The recipient calls `subscribe_events` with that string as `awaiting_agent`:

```json
{ "awaiting_agent": "reviewer-a", "since_seq": 42, "since_context": "build-box", "timeout_ms": 30000 }
```

Use your saved cursor for this scope and context as `since_seq`; use the source
`context.name` as `since_context`. The latter is required after switching contexts,
and a mismatched context is rejected. Context switches preserve session identity
and discard old connection subscriptions. Pass each response's
`last_seq` into the next poll. Omitting `since_seq` receives only live events.
`awaiting_agent`, `review_id`, and `workspace_id` are mutually exclusive filters.
The identity tools do not create or change subscription filters automatically.

Changing the model while keeping the name preserves the routing key. Changing
the name affects future authorship but does not rename previously addressed
requests; poll the old name with its saved cursor to finish old work if needed.
Names are labels, not unique session IDs or authentication credentials.
