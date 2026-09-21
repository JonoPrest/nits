// Git tracks these entry kinds, not arbitrary filesystem permission bits.
let mode = (mode: Domain.BlobMode.t) =>
  switch mode {
  | Regular => "100644"
  | Executable => "100755 (executable)"
  | Symlink => "120000 (symlink)"
  | UnknownMode => "unknown Git mode"
  }

let key = (entry: Domain.BlobEntry.t) => entry.oid ++ ":" ++ mode(entry.mode)

let change = (old: Domain.BlobEntry.t, new: Domain.BlobEntry.t) =>
  if old.mode != new.mode {
    Some(mode(old.mode) ++ " → " ++ mode(new.mode))
  } else if old.mode == UnknownMode {
    Some("Git mode unknown (historical)")
  } else {
    None
  }

let summary = (target: Render.RenderTarget.t) =>
  switch target {
  | Blob({entry}) => Some(mode(entry.mode))
  | Diff({change: Added({new})}) => Some("Added " ++ mode(new.mode))
  | Diff({change: Deleted({old})}) => Some("Removed " ++ mode(old.mode))
  | Diff({change: Modified({old, new}) | Renamed({old, new})}) => change(old, new)
  | Diff({change: Submodule(_)}) => None
  }

@react.component
let make = (~target: Render.RenderTarget.t) =>
  switch summary(target) {
  | Some(text) => <UI.Badge text />
  | None => React.null
  }
