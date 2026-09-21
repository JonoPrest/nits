let workspace = () => Fixtures.parse(Domain.Workspace.schema, "protocol", "Workspace", "default")
let make = (
  ~workspace: Domain.Workspace.t=workspace(),
  ~id="01ARZ3NDEKTSV4RRFFQ69G5FAA",
): View.ReviewCreation.t => {
  reviewId: id,
  workspaceId: workspace.id,
  context: Some(Named({name: "test"})),
  draft: {
    title: "",
    targets: workspace.repos
    ->Array.filterWithIndex((_, i) => i == 0)
    ->Array.map(repo => {
      View.CreationTarget.repoId: repo.id,
      id: 0,
      base: Automatic({}),
      head: "worktree",
    }),
  },
  defaults: workspace.repos->Array.map(repo => {
    View.CreationDefault.repoId: repo.id,
    state: Ready({base: Branch({name: "develop"})}),
  }),
  selected: None,
  revision: 0,
  status: Editing({}),
}
let hints: array<View.Hint.t> = [
  {command: ReconnectReviewCreation, keys: "alt+r", label: "reconnect review creation"},
  {command: Submit, keys: "ctrl+u", label: "submit"},
  {command: Back, keys: "ctrl+b", label: "back"},
  {command: AddReviewTarget, keys: "ctrl+a", label: "add target"},
  {command: RemoveReviewTarget, keys: "ctrl+r", label: "remove target"},
]
