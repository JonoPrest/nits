// Gitlinks carry commit identities; these are metadata, never source lines.
let text = (change: Domain.SubmoduleChange.t) =>
  switch change {
  | Added({new}) => "Submodule added\nNew commit: " ++ new
  | Deleted({old}) => "Submodule removed\nOld commit: " ++ old
  | Updated({old, new}) => "Submodule updated\nOld commit: " ++ old ++ "\nNew commit: " ++ new
  | Renamed({from, old, new}) =>
    "Submodule renamed from " ++ from ++ "\nOld commit: " ++ old ++ "\nNew commit: " ++ new
  | BlobToSubmodule({old, new}) =>
    "Blob replaced by submodule\nOld blob: " ++ old ++ "\nNew commit: " ++ new
  | SubmoduleToBlob({old, new}) =>
    "Submodule replaced by blob\nOld commit: " ++ old ++ "\nNew blob: " ++ new
  }

@react.component
let make = (~target: Render.RenderTarget.t) =>
  switch target {
  | Diff({change: Submodule({change})}) =>
    <div className="submodule-change" role="note" ariaLabel="Submodule change">
      <pre> {React.string(text(change))} </pre>
      <p> {React.string("Gitlink metadata has no line diff. Discuss this change in the review conversation.")} </p>
    </div>
  | Diff({change: Added(_) | Deleted(_) | Modified(_) | Renamed(_)}) | Blob(_) => React.null
  }
