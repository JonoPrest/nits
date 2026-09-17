// The commit list is the diff-scope picker: aggregate first, then commits,
// then the optional working-tree step. DiffScope is the single source of
// truth shared by shortcuts and pointer selection.

open View

let relative = (t: Ids.timestamp): string => {
  let seconds = (Date.now() -. t) /. 1000.
  let n = seconds->Float.toInt
  if n < 60 {
    "just now"
  } else if n < 3600 {
    Int.toString(n / 60) ++ " min ago"
  } else if n < 86400 {
    Int.toString(n / 3600) ++ " h ago"
  } else {
    Int.toString(n / 86400) ++ " d ago"
  }
}

let absolute = (t: Ids.timestamp): string => Date.fromTime(t)->Date.toISOString

module CommitPanel = {
  @react.component
  let make = (~commit: StepperCommit.t) =>
    <div className="commit-panel">
      <div className="commit-subject"> {React.string(commit.subject)} </div>
      {commit.body == ""
        ? React.null
        : <pre className="commit-body"> {React.string(commit.body)} </pre>}
      <dl className="commit-meta">
        <dt> {React.string("author")} </dt>
        <dd title={absolute(commit.time)}>
          {React.string(commit.author ++ ", " ++ relative(commit.time))}
        </dd>
        <dt> {React.string("committer")} </dt>
        <dd title={absolute(commit.committerTime)}>
          {React.string(commit.committer ++ ", " ++ relative(commit.committerTime))}
        </dd>
        <dt> {React.string("parents")} </dt>
        <dd>
          {commit.parents
          ->Array.map(p =>
            <span key=p className="commit-oid">
              {React.string(String.slice(p, ~start=0, ~end=8))}
            </span>
          )
          ->React.array}
        </dd>
      </dl>
    </div>
}

type row = AllChanges | CommitRow(StepperCommit.t) | WorkingTree

@react.component
let make = (
  ~stepper: CommitStepper.t,
  ~scope: Domain.DiffScope.t,
  ~focus: Focus.t,
  ~dispatch: Action.t => unit,
) => {
  let focusedIndex = switch focus {
  | CommitStepper({index}) => Some(index)
  | _ => None
  }
  let rows = Array.concat(
    [AllChanges],
    Array.concat(
      stepper.commits->Array.map(commit => CommitRow(commit)),
      stepper.hasWorktree ? [WorkingTree] : [],
    ),
  )
  let selected = switch scope {
  | Commit({repoId, oid}) if repoId == stepper.repoId =>
    stepper.commits->Array.find(commit => commit.oid == oid)
  | All(_) | Committed(_) | Commit(_) | Worktree(_) | Requested(_) | SinceCheckpoint(_) => None
  }
  <UI.Panel title="Commits">
    <ol role="list">
      {rows
      ->Array.mapWithIndex((row, i) => {
        let isSelected = switch (row, scope) {
        | (AllChanges, All(_) | Committed(_)) => true
        | (CommitRow(commit), Commit({repoId, oid})) =>
          repoId == stepper.repoId && oid == commit.oid
        | (WorkingTree, Worktree({repoId})) => repoId == stepper.repoId
        | (AllChanges, Commit(_) | Worktree(_))
        | (CommitRow(_), All(_) | Committed(_) | Worktree(_))
        | (WorkingTree, All(_) | Committed(_) | Commit(_)) => false
        | (AllChanges | CommitRow(_) | WorkingTree, Requested(_) | SinceCheckpoint(_)) => false
        }
        let choice = switch row {
        | AllChanges => Action.ScopeChoice.All({})
        | CommitRow(commit) => Action.ScopeChoice.Commit({repoId: stepper.repoId, oid: commit.oid})
        | WorkingTree => Action.ScopeChoice.Worktree({repoId: stepper.repoId})
        }
        let key = switch row {
        | AllChanges => "all-changes"
        | CommitRow(commit) => commit.oid
        | WorkingTree => "working-tree"
        }
        Attrs.focused(
          <li
            key
            className={"stepper-commit" ++ (isSelected ? " stepper-selected" : "")}
            onClick={_ => {
              dispatch(SetFocus({focus: Focus.CommitStepper({index: i})}))
              dispatch(SetScope({scope: choice}))
            }}
          >
            {switch row {
            | AllChanges => <span className="commit-subject"> {React.string("All changes")} </span>
            | CommitRow(commit) =>
              <>
                <span className="commit-oid">
                  {React.string(String.slice(commit.oid, ~start=0, ~end=8))}
                </span>
                <span className="commit-subject"> {React.string(commit.subject)} </span>
                <span className="commit-author"> {React.string(commit.author)} </span>
              </>
            | WorkingTree =>
              <span className="commit-subject"> {React.string("Working tree")} </span>
            }}
          </li>,
          focusedIndex == Some(i),
        )
      })
      ->React.array}
    </ol>
    {switch selected {
    | Some(commit) => <CommitPanel commit />
    | None => React.null
    }}
  </UI.Panel>
}
