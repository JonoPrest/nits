// Checked suggestion content, produced by the daemon's strict patch parser.
module LineKind = {
  @schema @tag("type")
  type t = Context({old: int, new: int}) | Remove({old: int}) | Add({new: int})
}
module Line = {
  @schema
  type t = {kind: LineKind.t, text: string, ending: Render.LineEnding.t}
}
module Hunk = {
  @schema
  type t = {header: string, lines: array<Line.t>}
}
module Worktree = {
  @@warning("-27")
  @schema @tag("type")
  type t = Original({}) | Proposed({}) | Changed({}) | Unavailable({reason: string})
  @@warning("+27")
}
module Inspection = {
  @schema @tag("type")
  type t = Checked({hunks: array<Hunk.t>, worktree: Worktree.t}) | Rejected({reason: string})
}
module Preview = {
  @schema
  type t = {suggestion: Domain.SuggestionRecord.t, inspection: Inspection.t}
}
