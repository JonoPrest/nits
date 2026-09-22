# Activating an installed Nits build

`nits daemon inspect --json` describes the executable being invoked without
reading configuration or opening a store. A build has a SHA-256 digest of its
complete executable, a release channel and semantic release version, its
application protocol and store schema, and separate maintenance and MCP-worker
protocol versions. Equal package versions or equal wire protocols do not imply
equal executable bytes.

Release builds set `NITS_BUILD_RELEASE` and `NITS_BUILD_CHANNEL` at compilation.
The defaults are the package version and `stable`. Automatic activation requires
a newer release on the same channel. An explicit activation also permits a
different development build with equal release precedence; neither path accepts
an older release, a different channel, or a candidate with an older store schema.

`nits daemon upgrade-status --json` reports the selected context's running build,
installed candidate and last operation. It never starts or restarts a daemon.
`nits daemon upgrade --json` activates the verified installed candidate. An
`Accepted` result contains an operation ID and phase; it is not a readiness
claim. Public `daemon upgrade` prints its typed result and exits nonzero for
`Failed`; status queries and accepted operations retain a successful exit.
Inspect status until the operation becomes `Ready` or `Failed`.
`AlreadyCurrent` means the running and selected installed digests match.

Local contexts use the selected Nits installation (`NITS_BIN`, the invoked CLI
path, or the host's normal binary discovery). SSH contexts inspect and activate
their configured executable on the remote machine. A local installation does
not prove the remote installation was updated. Raw WebSocket contexts report
`NotManaged`; their serving process must be managed separately. Named contexts,
persisted defaults and explicit `require-running` connection policy remain
unchanged by a status query or reconnect.

A newer managed client automatically repairs an incompatible older daemon when
its start policy permits it. A compatible daemon keeps serving until explicit
activation. A stale client cannot select an older release and downgrade the
running daemon.

## What a restart owns

A maintenance socket uses its own versioned contract, independently of the
application Hello protocol. One coordinator holds a persistent per-store lock,
records the source/target build and operation ID, freezes verified replacement
bytes, and asks the incumbent to stop admitting ordinary work. The incumbent
sends a restart notice independently of review subscription filters. Already
accepted jobs keep their admission permit and actual store ownership until the
work finishes, even if the requesting client disconnects.

Only the admitted coordinator updates shared operation progress. A competing
request that fails revalidation keeps its own terminal receipt, so it cannot
replace the winning operation's readiness or strand clients waiting for it.

Connections receive completed receipts where possible. Long requests receive
an explicit interruption, and output draining has a deadline so a slow reader
cannot hold the daemon indefinitely. A closed listening socket is not proof
that Git work released the store. Replacement starts only after ownership is
free, then must pass both expected-build inspection and an application handshake.
The actual data directory, Unix socket, WebSocket listener and idle setting are
preserved.

Preflight failure leaves the incumbent serving. A drain timeout does not kill
its accepted work. Startup or migration failure reports that phase and retains
the store and operation record. There is no automatic rollback after migration.
Inspect `nitsd.log` in the data directory and `daemon upgrade-status`; after
remaining work releases ownership, correct the installation/configuration and
use `daemon start` to recover a stopped endpoint.

## Connected clients and uncertain work

Compatible browser/desktop hosts reconnect with bounded backoff, retaining the
review, navigation, draft and last received cursor. An incompatible host shows
an upgrade-required state and retains that recovery information. Refreshing a
page does not replace an old process serving the browser bridge; update/restart
the bridge or desktop application itself.

A lost mutation reply is an unknown outcome. The client reconciles its durable
events and does not blindly resend the mutation. Only a typed rejection proving
that work was never admitted permits automatic resend. Suggestion application
keeps its own receipt/preview reconciliation and requires an explicit new apply
when the original result cannot be confirmed.

`events --follow` reconnects to its original context and resumes its bounded
replay position after fully flushed output. A protocol change requiring a new
CLI produces an explicit upgrade-client error; rerun the installed CLI with the
last printed cursor. Non-streaming CLI mutations are never replayed.

## Legacy boundary

Builds predating this maintenance contract cannot announce a restart using a
message their strict clients do not understand. They return a bounded,
actionable bootstrap requirement: use the supported manual `daemon stop`, wait
for its ownership-aware completion, then `daemon start` from the installed
build. An already-running legacy MCP adapter predating the stable supervisor
also needs one host restart to enter the supported upgrade contract. This
bootstrap does not claim graceful notices or transparent session replacement
from binaries that never implemented them.

## An ongoing MCP session

`get_daemon_status` remains available even if the application Hello is rejected.
It reports the selected context, daemon/candidate, stable MCP host and active
worker builds, compatibility and operation progress. `restart_daemon` accepts
no executable path: it targets that selected managed context and verified
installation. Pings, cancellation and independent status remain responsive.

The host runs a replaceable worker over a separate versioned private protocol.
It checkpoints initialized identity, invoking human, selected context and cursor
policy before acknowledging session changes. A new worker restores that state
without dialing during restore, then obtains a fresh daemon ClientId and sequence.
In-flight requests and mutation identities are never replayed into that worker.
Interrupted writes report unknown outcome; explicitly unforwarded work is
identified separately. Tool manifests come from the typed worker contract, and
capability changes emit the MCP tool-list change notification.

Before activation, the supervisor verifies a compatible local worker candidate
and pins the remote/local daemon candidate's digest. An installation replacement
between those steps fails before disrupting the incumbent. SSH needs the proper
remote daemon installation and local worker installation; a remote-only update
does not magically update an old local adapter. The host remains usable for
management and returns an explicit upgrade requirement if it cannot load a
compatible worker. Readiness requires the target build to be running, not merely
a historical Ready journal entry.
