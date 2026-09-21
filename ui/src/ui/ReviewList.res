// Core-owned home rows give keys and clicks the same workspace/repo/review identity.
open View
@react.component
let make = (
  ~reviews: array<Domain.Review.t>,
  ~workspaces: array<Domain.Workspace.t>,
  ~home: HomeView.t,
  ~focus: Focus.t,
  ~chrome: array<Hint.t>,
  ~dispatch: Action.t => unit,
) => {
  let indexOf = (workspaceId, kind) => {
    let index =
      home.rows->Array.findIndex(row => row.workspaceId == workspaceId && row.kind == kind)
    index < 0 ? 0 : index
  }
  let isFocused = (index: int) =>
    switch focus {
    | ReviewList({index: focused}) => focused == index
    | _ => false
    }
  let select = (index: int) => dispatch(SetFocus({focus: Focus.ReviewList({index: index})}))
  <UI.Panel
    title="Workspaces"
    actions={<UI.Button
      label="Refresh"
      kind=Ghost
      title=?{Chrome.tip(chrome, Refresh)}
      onClick={() => dispatch(ListWorkspaces({}))}
    />}
  >
    {Array.length(workspaces) == 0 ? <UI.Empty text="No workspaces yet." /> : React.null}
    {workspaces
    ->Array.map(workspace => {
      let index = indexOf(workspace.id, Workspace({}))
      let expanded = home.expanded->Array.includes(workspace.id)
      let label = RepositoryIdentity.workspaceLabel(workspaces, workspace)
      <section key=workspace.id className="workspace-group" ariaLabel=label>
        {Attrs.focused(
          <header className="workspace-header">
            <UI.Button
              label={(expanded ? "▾ " : "▸ ") ++ label}
              kind=Ghost
              expanded
              ariaLabel={"Repositories in " ++ label}
              title=?{Chrome.tip(chrome, Open)}
              onClick={() => {
                select(index)
                dispatch(RunCommand({command: Open}))
              }}
            />
            <UI.Badge
              text={RepositoryIdentity.count(
                ~total=Array.length(workspace.repos),
                ~singular="repo",
                ~plural="repos",
              )}
            />
            <UI.Button
              label="+"
              kind=Ghost
              ariaLabel={"New review in " ++ label}
              title=?{Chrome.tip(chrome, NewReview)}
              onClick={() => {
                select(index)
                dispatch(StartReview({workspaceId: workspace.id}))
              }}
            />
          </header>,
          isFocused(index),
        )}
        {expanded
          ? <ul className="workspace-inventory" ariaLabel={label ++ " repository inventory"}>
              {workspace.repos
              ->Array.map(repository => {
                let index = indexOf(workspace.id, Repository({repoId: repository.id}))
                Attrs.focused(
                  <li key=repository.id className="workspace-repo" onClick={_ => select(index)}>
                    <span className="workspace-repo-name">
                      {React.string(RepositoryIdentity.repoTitle(workspace, repository))}
                    </span>
                    <code className="checkout-path"> {React.string(repository.path)} </code>
                    <UI.Button
                      label="Copy path"
                      kind=Ghost
                      title=?{Chrome.tip(chrome, CopyCheckout)}
                      onClick={() => {
                        select(index)
                        dispatch(CopyCheckout({repoId: repository.id}))
                      }}
                    />
                  </li>,
                  isFocused(index),
                )
              })
              ->React.array}
              {Array.length(workspace.repos) == 0
                ? <li>
                    <UI.Empty text="No repositories attached." />
                  </li>
                : React.null}
            </ul>
          : React.null}
        <ul ariaLabel={label ++ " reviews"}>
          {reviews
          ->Array.filter(r => r.workspaceId == workspace.id)
          ->Array.map(review => {
            let index = indexOf(workspace.id, Review({reviewId: review.id}))
            Attrs.focused(
              <li
                key=review.id
                className="review-item"
                onClick={_ => {
                  select(index)
                  dispatch(OpenReview({reviewId: review.id}))
                }}
              >
                <span className="review-title"> {React.string(review.title)} </span>
                {review.status == Archived ? <UI.Badge text="archived" /> : React.null}
                <RepositoryIdentity.Targets review workspace />
              </li>,
              isFocused(index),
            )
          })
          ->React.array}
        </ul>
      </section>
    })
    ->React.array}
  </UI.Panel>
}
