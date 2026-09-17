// Identifiers and scalars (nits-protocol `ids.rs`, `version.rs`). All
// serde-transparent on the wire: ULIDs and OIDs are strings, counters are
// numbers. u64 counters may exceed 2^31, so they are floats here.

@schema type workspaceId = string
@schema type repoId = string
@schema type reviewId = string
@schema type reviewRequestId = float
@schema type commentId = string
@schema type threadId = string
@schema type clientId = string

@schema type blobOid = string
@schema type commitOid = string
@schema type treeOid = string

@schema type seq = float
@schema type clientSeq = float
@schema type requestId = float
/// Milliseconds since the Unix epoch.
@schema type timestamp = float

@schema type protocolVersion = string
@schema type schemaVersion = int
