// Print typed references without losing their explicit namespace.
// Input parsing belongs to the shared Rust core boundary.

open Domain

let print = (spec: RefSpec.t): string =>
  switch spec {
  | Branch({name}) => "branch:" ++ name
  | Revision({expression}) => expression
  | Tag({name}) => "tag:" ++ name
  | Commit({oid}) => "commit:" ++ oid
  | WorkingTree(_) => "worktree"
  | Head(_) => "head"
  | Upstream(_) => "upstream"
  }
