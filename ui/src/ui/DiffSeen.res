// Rows already seen for a file (shared by DiffView and FileDiff): patches
// are viewport-bounded (§6.3), so the UI accumulates rows per file and a
// rendered row never regresses to a placeholder while scrolling.

/// Identity of the rendered file; cached rows are dropped when it changes
/// (another file, a re-render with new totals).
let fileKey = (diff: View.DiffView.t): string =>
  switch diff.target {
  | Blob({oid}) => oid
  | Diff({change}) =>
    switch change {
    | Added({new}) => "added:" ++ new
    | Deleted({old}) => "deleted:" ++ old
    | Modified({old, new}) => old ++ ":" ++ new
    | Renamed({from, old, new}) => from ++ ":" ++ old ++ ":" ++ new
    }
  } ++
  "\x00" ++
  diff.file.repoId ++
  "\x00" ++
  diff.file.path ++
  switch diff.content {
  | Text({totalRows, additions, deletions}) =>
    "\x00" ++
    Int.toString(totalRows) ++
    "\x00" ++
    Int.toString(additions) ++
    "\x00" ++
    Int.toString(deletions)
  | Binary(_) => "\x00binary"
  }

/// Merge the patch's viewport rows into the rows seen so far. Clears when
/// the file identity changes.
let mergeSeen = (
  seen: Dict.t<View.DiffRow.t>,
  prevKey: string,
  key: string,
  rows: array<View.DiffRow.t>,
): Dict.t<View.DiffRow.t> => {
  let out = prevKey == key ? seen : Dict.make()
  rows->Array.forEach(r => out->Dict.set(Int.toString(r.index), r))
  out
}
