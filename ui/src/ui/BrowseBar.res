// The candidate repository is explicit; the visible revision changes only
// after the core successfully resolves it.
@react.component
let make = (
  ~browse: View.BrowseView.t,
  ~targets: array<Domain.ReviewTarget.t>,
  ~repositories: RepositoryIdentity.context,
  ~chrome: array<View.Hint.t>,
  ~disabled=false,
  ~dispatch: Action.t => unit,
) => {
  let label = (target: View.BrowseTarget.t) =>
    RepositoryIdentity.title(repositories, target.repoId) ++
    " · " ++
    RefSpecText.print(target.refSpec)
  let visible = switch browse.selection {
  | Some(target) => label(target)
  | None =>
    targets
    ->Array.map(target =>
      RepositoryIdentity.title(repositories, target.repoId) ++
      " · " ++
      RefSpecText.print(target.head)
    )
    ->Array.join(", ")
  }
  <div className="browse-bar">
    <span className="browse-viewing"> {React.string("Viewing: " ++ visible)} </span>
    <label title=?{Chrome.tip(chrome, NextBrowseRepo)}>
      <span> {React.string("Repository ")} </span>
      <UI.Select
        value=browse.repoId
        options={targets->Array.map(target => (
          target.repoId,
          RepositoryIdentity.description(repositories, target.repoId),
        ))}
        ariaLabel="Browse repository"
        disabled
        onChange={repoId => dispatch(SelectBrowseRepo({repoId: repoId}))}
      />
    </label>
    <UI.Button
      label="Choose revision…"
      title=?{Chrome.tip(chrome, BrowseRevision)}
      disabled
      onClick={() => dispatch(RunCommand({command: BrowseRevision}))}
    />
    <UI.Button
      label="Review heads"
      kind=Ghost
      title=?{Chrome.tip(chrome, ResetBrowse)}
      disabled
      onClick={() => dispatch(ResetBrowse({}))}
    />
    {switch browse.attempt {
    | None => React.null
    | Some({target, status: Loading(_)}) =>
      <span role="status"> {React.string("Loading " ++ label(target) ++ "…")} </span>
    | Some({target, status: Failed({message})}) =>
      <span className="browse-bad" role="alert">
        {React.string("Could not open " ++ label(target) ++ ": " ++ message)}
      </span>
    }}
  </div>
}
