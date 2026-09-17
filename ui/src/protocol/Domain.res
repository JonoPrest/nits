// Domain types (nits-protocol `domain.rs`, `invariants.rs`), one module per
// Rust type, schemas derived by `@schema`. Payload enums are `type`-tagged
// objects (`@tag`), unit-only enums are bare strings, `Option` is null.

open Ids

module Repo = {
  @schema
  type t = {id: repoId, path: string, @as("display_name") displayName: string}
}

module Workspace = {
  @schema
  type t = {id: workspaceId, name: string, repos: array<Repo.t>}
}

module Human = {
  @schema
  type t = {name: string, machine: string}
}

module AgentVia = {
  @schema
  type t = Cli | Mcp
}

module Author = {
  @schema @tag("type")
  type t =
    | @as("Human") Human({name: string, machine: string})
    | @as("Agent")
    Agent({
        name: string,
        model: string,
        @as("session_id") sessionId: string,
        @as("invoked_by") invokedBy: @s.null option<Human.t>,
        via: AgentVia.t,
      })
    | @as("Daemon") Daemon({machine: string})
}

module RefSpec = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("Branch") Branch({name: string})
    | @as("Commit") Commit({oid: commitOid})
    | @as("Tag") Tag({name: string})
    | @as("WorkingTree") WorkingTree({})
    | @as("Upstream") Upstream({})
    | @as("Head") Head({})
  @@warning("+27")
}

module BaseRefSpec = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("Branch") Branch({name: string})
    | @as("Commit") Commit({oid: commitOid})
    | @as("Tag") Tag({name: string})
    | @as("Upstream") Upstream({})
    | @as("Head") Head({})
  @@warning("+27")
}

module TargetRevision = {
  @schema @tag("type")
  type t =
    | @as("Base") Base({@as("ref_spec") refSpec: BaseRefSpec.t})
    | @as("Head") Head({@as("ref_spec") refSpec: RefSpec.t})
}

module ReviewTargetUpdate = {
  @schema
  type t = {@as("repo_id") repoId: repoId, revision: TargetRevision.t}
}

module RefCandidate = {
  @schema
  type t = {@as("ref_spec") refSpec: RefSpec.t, subject: @s.null option<string>}
}

module ResolvedSource = {
  @schema @tag("type")
  type t =
    | @as("Commit") Commit({oid: commitOid})
    | @as("WorkingTree") WorkingTree({dirty: array<string>, branch: @s.null option<string>})
}

module ResolvedRef = {
  @schema
  type t = {tree: treeOid, source: ResolvedSource.t}
}

module ReviewTarget = {
  @schema
  type t = {@as("repo_id") repoId: repoId, base: RefSpec.t, head: RefSpec.t}
}

module ResolvedTarget = {
  @schema
  type t = {@as("repo_id") repoId: repoId, base: ResolvedRef.t, head: ResolvedRef.t}
}

module ReviewStatus = {
  @schema
  type t = Open | Archived
}

module Review = {
  @schema
  type t = {
    id: reviewId,
    @as("workspace_id") workspaceId: workspaceId,
    title: string,
    targets: array<ReviewTarget.t>,
    created: timestamp,
    status: ReviewStatus.t,
  }
}

module Sig = {
  @schema
  type t = {name: string, email: string, time: timestamp, @as("offset_minutes") offsetMinutes: int}
}

module CommitInfo = {
  @schema
  type t = {
    oid: commitOid,
    parents: array<commitOid>,
    tree: treeOid,
    author: Sig.t,
    committer: Sig.t,
    subject: string,
    body: string,
  }
}

module Side = {
  @schema
  type t = Base | Head

  /// The wire spelling, also used as the `data-side` attribute.
  let name = (side: t): string =>
    switch side {
    | Base => "Base"
    | Head => "Head"
    }
}

/// 16 hex chars.
@schema type contextHash = string

module LineRange = {
  /// 1-based, inclusive.
  @schema
  type t = {start: int, @as("end") end_: int}
}

module Anchor = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("Review") Review({})
    | @as("File")
    File({
        @as("repo_id") repoId: repoId,
        path: string,
        @as("blob_oid") blobOid: blobOid,
      })
    | @as("Lines")
    Lines({
        @as("repo_id") repoId: repoId,
        path: string,
        side: Side.t,
        @as("blob_oid") blobOid: blobOid,
        lines: LineRange.t,
        @as("context_hash") contextHash: contextHash,
      })
  @@warning("+27")
}

module CommentIntent = {
  @schema
  type t = Finding | Informational
}

module CommentKind = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("Note") Note({})
    | @as("Informational") Informational({})
    | @as("Suggestion") Suggestion({patch: string})
    | @as("Request") Request({})
  @@warning("+27")
}

module ContentHit = {
  @schema
  type t = {
    @as("repo_id") repoId: repoId,
    path: string,
    line: int,
    text: string,
  }
}

module DiffScope = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("All") All({})
    | @as("Committed") Committed({})
    | @as("Commit") Commit({@as("repo_id") repoId: repoId, oid: commitOid})
    | @as("Worktree") Worktree({@as("repo_id") repoId: repoId})
  @@warning("+27")
}

module ChangeKind = {
  @schema @tag("type")
  type t =
    | @as("Added") Added({new: blobOid})
    | @as("Deleted") Deleted({old: blobOid})
    | @as("Modified") Modified({old: blobOid, new: blobOid})
    | @as("Renamed") Renamed({from: string, old: blobOid, new: blobOid})
}

module CommentState = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("Live") Live({})
    | @as("Outdated") Outdated({@as("last_good_anchor") lastGoodAnchor: Anchor.t})
    | @as("Deleted") Deleted({})
  @@warning("+27")
}

module Comment = {
  @schema
  type t = {
    id: commentId,
    @as("review_id") reviewId: reviewId,
    @as("thread_id") threadId: threadId,
    author: Author.t,
    kind: CommentKind.t,
    anchor: Anchor.t,
    body: string,
    created: timestamp,
    edited: @s.null option<timestamp>,
    state: CommentState.t,
    context: @s.null option<ChangeKind.t>,
  }
}

module ThreadResolution = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("Open") Open({})
    | @as("Resolved") Resolved({by: Author.t, at: timestamp})
    | @as("Informational") Informational({})
  @@warning("+27")
}

module Thread = {
  @schema
  type t = {
    id: threadId,
    @as("review_id") reviewId: reviewId,
    root: commentId,
    replies: array<commentId>,
    resolution: ThreadResolution.t,
  }
}

module ViewedMark = {
  @schema
  type t = {
    @as("review_id") reviewId: reviewId,
    @as("repo_id") repoId: repoId,
    path: string,
    viewer: Human.t,
    @as("blob_oid") blobOid: @s.null option<blobOid>,
  }
}

/// How far one hidden run has been pushed apart: `up` from the hunk
/// below it, `down` from the hunk above it.
module GapExpansion = {
  @schema
  type t = {gap: int, up: int, down: int}
}

module RenderOpts = {
  @schema
  type t = {
    @as("ignore_whitespace") ignoreWhitespace: bool,
    @as("context_lines") contextLines: int,
    /// The gaps the reader has opened, one at a time.
    expanded: array<GapExpansion.t>,
  }
}

module FileChange = {
  @schema
  type t = {@as("repo_id") repoId: repoId, path: string, kind: ChangeKind.t}
}

module TreeEntryKind = {
  @schema @tag("type")
  type t =
    | @as("File") File({oid: blobOid, size: float, executable: bool})
    | @as("Dir") Dir({oid: treeOid})
    | @as("Symlink") Symlink({oid: blobOid})
    | @as("Submodule") Submodule({commit: commitOid})
}

module TreeEntry = {
  @schema
  type t = {path: string, kind: TreeEntryKind.t}
}

module TreeSnapshot = {
  @schema
  type t = {
    @as("repo_id") repoId: repoId,
    @as("root_oid") rootOid: treeOid,
    entries: array<TreeEntry.t>,
  }
}

module TreeDelta = {
  @schema
  type t = {
    @as("repo_id") repoId: repoId,
    @as("from_root") fromRoot: treeOid,
    @as("to_root") toRoot: treeOid,
    added: array<TreeEntry.t>,
    removed: array<string>,
    changed: array<TreeEntry.t>,
  }
}

module ReviewSnapshot = {
  @schema
  type t = {
    review: Review.t,
    resolved: @s.null option<array<ResolvedTarget.t>>,
    threads: array<Thread.t>,
    comments: array<Comment.t>,
    viewed: array<ViewedMark.t>,
    seq: seq,
  }
}
