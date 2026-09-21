// Render model (nits-protocol `render.rs`).

open Ids

module SpanClass = {
  @schema
  type t =
    | Keyword
    | String
    | Number
    | Comment
    | Type
    | Function
    | Variable
    | Constant
    | Operator
    | Punctuation
    | Attribute
    | Tag
    | Other
}

module ColRange = {
  /// Byte offsets, end exclusive.
  @schema
  type t = {start: int, @as("end") end_: int}
}

module Span = {
  @schema
  type t = {range: ColRange.t, class: SpanClass.t}
}

module Cell = {
  @schema
  type t = {
    @as("line_no") lineNo: int,
    text: string,
    spans: array<Span.t>,
    changed: array<ColRange.t>,
  }
}

module ExpandDir = {
  @schema
  type t = Up | Down | Both
}

module Row = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("HunkHeader") HunkHeader({text: string})
    | @as("Context") Context({left: Cell.t, right: Cell.t})
    | @as("Removed") Removed({left: Cell.t})
    | @as("Added") Added({right: Cell.t})
    | @as("Modified") Modified({left: Cell.t, right: Cell.t})
    | @as("Expander") Expander({hidden: int, dir: ExpandDir.t, gap: int})
    | @as("WhitespaceOnly") WhitespaceOnly({})
  @@warning("+27")
}

@schema type chunkIndex = int

module RenderTarget = {
  @schema @tag("type")
  type t =
    | @as("Diff") Diff({change: Domain.ChangeKind.t})
    | @as("Blob") Blob({entry: Domain.BlobEntry.t})
}

/// One gap's expander row: which gap, and the row it is on.
module GapRow = {
  @schema
  type t = {gap: int, row: int}
}

module RenderContent = {
  @@warning("-27")
  @schema @tag("type")
  type t =
    | @as("Binary") Binary({})
    | @as("Submodule") Submodule({})
    | @as("Text")
    Text({
        @as("total_rows") totalRows: int,
        @as("chunk_rows") chunkRows: int,
        @as("chunk_count") chunkCount: int,
        highlighted: bool,
        additions: int,
        deletions: int,
        /// Where each still-hidden run's expander sits, in row order.
        gaps: array<GapRow.t>,
      })
  @@warning("+27")
}

module FileRenderHeader = {
  @schema
  type t = {
    @as("repo_id") repoId: repoId,
    path: string,
    target: RenderTarget.t,
    opts: Domain.RenderOpts.t,
    lang: @s.null option<string>,
    content: RenderContent.t,
  }
}

module RenderChunk = {
  @schema
  type t = {index: chunkIndex, rows: array<Row.t>}
}

module FileRender = {
  @schema
  type t = {header: FileRenderHeader.t, chunks: array<RenderChunk.t>}
}

module FileSummary = {
  @schema
  type t = {
    @as("repo_id") repoId: repoId,
    path: string,
    change: Domain.ChangeKind.t,
    additions: int,
    deletions: int,
    binary: bool,
  }
}

module DiffSummary = {
  @schema
  type t = {files: array<FileSummary.t>, additions: int, deletions: int}
}
