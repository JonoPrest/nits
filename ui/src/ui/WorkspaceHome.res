// Details remain useful with no reviews or with a hidden sidebar.
@react.component
let make = (~model: View.ViewModel.t, ~dispatch: Action.t => unit) => {
  let cli = RepositoryIdentity.cliPrefix(model.daemonContext)
  let focused = switch model.focus {
  | ReviewList({index}) => model.home.rows->Array.get(index)
  | _ => None
  }
  let repoFocused = repoId =>
    focused->Option.mapOr(false, row =>
      switch row.kind {
      | Repository({repoId: selected}) => selected == repoId
      | _ => false
      }
    )
  let reviewFocused = reviewId =>
    focused->Option.mapOr(false, row =>
      switch row.kind {
      | Review({reviewId: selected}) => selected == reviewId
      | _ => false
      }
    )
  let workspace =
    model.home.selectedWorkspace->Option.flatMap(id =>
      model.workspaces->Array.find(w => w.id == id)
    )
  <section className="workspace-home" ariaLabel="Workspace details">
    <RepositoryIdentity.Context context=model.daemonContext />
    {switch workspace {
    | None =>
      <>
        <h1> {React.string("Your workspaces")} </h1>
        <p> {React.string("A workspace groups the repositories you review together.")} </p>
        <p> {React.string("Create one on the selected daemon, then attach a checkout:")} </p>
        <pre className="workspace-setup">
          {React.string(
            cli ++
            " workspace add <name>\n" ++
            cli ++ " workspace attach <workspace-id> <checkout-path>",
          )}
        </pre>
        <UI.Button
          label="Refresh workspaces"
          title=?{Chrome.tip(model.chrome, Refresh)}
          onClick={() => dispatch(ListWorkspaces({}))}
        />
      </>
    | Some(workspace) =>
      <>
        <div className="workspace-detail-heading">
          <h1> {React.string(RepositoryIdentity.workspaceLabel(model.workspaces, workspace))} </h1>
          <UI.Badge
            text={RepositoryIdentity.count(
              ~total=Array.length(workspace.repos),
              ~singular="repository",
              ~plural="repositories",
            )}
          />
          <UI.Button
            label="New review"
            title=?{Chrome.tip(model.chrome, NewReview)}
            disabled={Array.length(workspace.repos) == 0}
            onClick={() => dispatch(StartReview({workspaceId: workspace.id}))}
          />
        </div>
        <p>
          {React.string(
            "All repositories in this workspace. Checkout paths belong to the daemon shown above.",
          )}
        </p>
        {Array.length(workspace.repos) == 0
          ? <>
              <h2> {React.string("Attach your first repository")} </h2>
              <pre className="workspace-setup">
                {React.string(cli ++ " workspace attach " ++ workspace.id ++ " <checkout-path>")}
              </pre>
              <UI.Button
                label="Refresh workspaces"
                title=?{Chrome.tip(model.chrome, Refresh)}
                onClick={() => dispatch(ListWorkspaces({}))}
              />
            </>
          : <ul className="workspace-repository-cards">
              {workspace.repos
              ->Array.map(repo =>
                Attrs.focused(
                  <li key=repo.id className="workspace-repository-card">
                    <strong> {React.string(RepositoryIdentity.repoTitle(workspace, repo))} </strong>
                    <code className="checkout-path"> {React.string(repo.path)} </code>
                    <UI.Button
                      label="Copy path"
                      ariaLabel={"Copy checkout path for " ++
                      RepositoryIdentity.repoLabel(workspace, repo.id)}
                      title=?{Chrome.tip(model.chrome, CopyCheckout)}
                      onClick={() => dispatch(CopyCheckout({repoId: repo.id}))}
                    />
                  </li>,
                  repoFocused(repo.id),
                )
              )
              ->React.array}
            </ul>}
        <h2> {React.string("Reviews")} </h2>
        {model.reviews->Array.filter(r => r.workspaceId == workspace.id)->Array.length == 0
          ? <p>
              {React.string(
                "No reviews yet. Create a review to choose which repositories and revisions to compare.",
              )}
            </p>
          : <ul className="workspace-review-cards">
              {model.reviews
              ->Array.filter(r => r.workspaceId == workspace.id)
              ->Array.map(review =>
                Attrs.focused(
                  <li key=review.id className="workspace-review-card">
                    <UI.Button
                      label=review.title
                      title=?{Chrome.tip(model.chrome, Open)}
                      onClick={() => dispatch(OpenReview({reviewId: review.id}))}
                    />
                    <RepositoryIdentity.Targets review workspace />
                  </li>,
                  reviewFocused(review.id),
                )
              )
              ->React.array}
            </ul>}
      </>
    }}
    {switch model.home.creating->Option.flatMap(id =>
      model.workspaces->Array.find(w => w.id == id)
    ) {
    | Some(workspace) =>
      <NewReview workspaces=[workspace] onClose={() => dispatch(CancelNewReview({}))} dispatch />
    | None => React.null
    }}
  </section>
}
