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

// Names belong to the open review's workspace, never the first global RepoId match.
type context = Unavailable | Workspace(Domain.Workspace.t)
let ofView = (model: View.ViewModel.t): context => {
  let review = switch model.review {
  | Some(open_) if model.openReview == Some(open_.snapshot.review.id) => Some(open_.snapshot.review)
  | Some(_) | None => model.reviews->Array.find(r => model.openReview == Some(r.id))
  }
  switch review->Option.flatMap(r => model.workspaces->Array.find(w => w.id == r.workspaceId)) {
  | Some(workspace) => Workspace(workspace)
  | None => Unavailable
  }
}
let title = (context, id) =>
  switch context {
  | Unavailable => "Repository · " ++ shortId(id)
  | Workspace(workspace) =>
    switch repo(workspace, id) {
    | Some(repository) => repoTitle(workspace, repository)
    | None => repoLabel(workspace, id)
    }
  }
let description = (context, id) =>
  switch context {
  | Unavailable => title(context, id)
  | Workspace(workspace) => repoLabel(workspace, id)
  }
let fileText = (context, file: View.FileRef.t) => title(context, file.repoId) ++ " · " ++ file.path
let fileDescription = (context, file: View.FileRef.t) =>
  description(context, file.repoId) ++ " · " ++ file.path
module File = {
  @react.component
  let make = (~repositories: context, ~file: View.FileRef.t) =>
    <span className="repository-file" title={fileDescription(repositories, file)}>
      <UI.Badge text={title(repositories, file.repoId)} />
      <span className="file-path mono"> {React.string(file.path)} </span>
    </span>
}

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
