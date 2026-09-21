// Shared identity for home, review targets and future repository pickers.
let shortId = id => String.slice(id, ~start=String.length(id) - 8, ~end=String.length(id))
let repo = (workspace: Domain.Workspace.t, id: Ids.repoId) =>
  workspace.repos->Array.find(repo => repo.id == id)
let repoLabel = (workspace: Domain.Workspace.t, id: Ids.repoId) =>
  switch repo(workspace, id) {
  | None => "Detached repository · " ++ shortId(id)
  | Some(repository) =>
    let duplicates = workspace.repos->Array.filter(r => r.displayName == repository.displayName)
    if Array.length(duplicates) > 1 {
      repository.displayName ++ " · " ++ repository.path ++ " · " ++ shortId(id)
    } else {
      repository.displayName
    }
  }
let repoTitle = (workspace: Domain.Workspace.t, repository: Domain.Repo.t) =>
  workspace.repos->Array.filter(r => r.displayName == repository.displayName)->Array.length > 1
    ? repository.displayName ++ " · " ++ shortId(repository.id)
    : repository.displayName

let count = (~total: int, ~singular: string, ~plural: string) =>
  Int.toString(total) ++ " " ++ (total == 1 ? singular : plural)

let workspaceLabel = (workspaces: array<Domain.Workspace.t>, workspace: Domain.Workspace.t) =>
  workspaces->Array.filter(w => w.name == workspace.name)->Array.length > 1
    ? workspace.name ++ " · " ++ shortId(workspace.id)
    : workspace.name
let contextLabel = (context: option<View.DaemonContext.t>) =>
  switch context {
  | Some(Named({name})) => "Daemon context: " ++ name
  | Some(Socket({path})) => "Daemon socket: " ++ path
  | Some(WebSocket({url})) => "Daemon: " ++ url
  | None => "Daemon context unavailable"
  }
// CLI guidance keeps the displayed daemon selection and quotes arbitrary names.
let shellQuote = value => "'" ++ value->String.split("'")->Array.join("'\"'\"'") ++ "'"
let cliPrefix = context =>
  switch context {
  | Some(View.DaemonContext.Named({name})) => "nits --context " ++ shellQuote(name)
  | Some(Socket({path})) => "nits --socket " ++ shellQuote(path)
  | Some(WebSocket({url})) => "nits --daemon-url " ++ shellQuote(url)
  | None => "nits"
  }

module Context = {
  @react.component
  let make = (~context: option<View.DaemonContext.t>) =>
    <span className="daemon-context"> {React.string(contextLabel(context))} </span>
}
module Targets = {
  @react.component
  let make = (~review: Domain.Review.t, ~workspace: Domain.Workspace.t) =>
    <span className="review-target-summary" ariaLabel="Repositories in this review">
      {review.targets
      ->Array.map(target =>
        <span key=target.repoId className="review-target-summary-item">
          <span className="target-repo-name">
            {React.string(repoLabel(workspace, target.repoId))}
          </span>
          <span>
            {React.string(
              RefSpecText.print(target.base) ++ " → " ++ RefSpecText.print(target.head),
            )}
          </span>
        </span>
      )
      ->React.array}
    </span>
}
