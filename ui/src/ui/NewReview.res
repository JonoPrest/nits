// Editable text stays responsive locally; the core owns defaults, validation,
// pending writes, errors and reconciliation. Echoes never overwrite later edits.
@val @scope("document") external activeElement: Nullable.t<Dom.element> = "activeElement"
let restoreFocus: (Dom.element, bool) => unit = %raw(`(element, preventScroll) => {
  // Disabling the submitted input moves native focus to body. A user who
  // deliberately focused another control while waiting keeps that choice.
  if (document.activeElement === document.body && element.isConnected && !element.disabled) {
    element.focus({preventScroll});
  }
}`)

@react.component
let make = (
  ~creation: View.ReviewCreation.t,
  ~workspaces: array<Domain.Workspace.t>,
  ~chrome: array<View.Hint.t>,
  ~bindings: array<View.Hint.t>,
  ~connection: View.ConnectionView.t=Subscribed({}),
  ~dispatch: Action.t => unit,
) => {
  let local = React.useRef(CreationRecovery.make())
  let (draft, setDraft) = React.useState(() => creation.draft)
  let (submitted, setSubmitted) = React.useState(() => false)
  let returnFocus = React.useRef(Nullable.null)
  let submitRef = React.useRef(Nullable.null)
  let pendingKeys = React.useRef(KeySequence.make())
  React.useEffect1(() => {
    CreationRecovery.observe(local.current, Some(creation))
    let current =
      local.current.snapshot->Option.map(s => s.creation.draft)->Option.getOr(creation.draft)
    setDraft(_ => current)
    // The incoming status can predate a locally submitted retry. Only the
    // reconciled snapshot may release the pending form, including on failure.
    let frozen =
      local.current.snapshot
      ->Option.map(saved =>
        saved.resume == Submitted &&
          switch saved.creation.status {
          | Interrupted(_) => false
          | Editing(_) | Failed(_) | Pending(_) | Reconciling(_) | Succeeded(_) => true
          }
      )
      ->Option.getOr(false)
    setSubmitted(_ => frozen)
    None
  }, [creation])
  let editable = View.ReviewCreation.editable(creation) && !submitted
  React.useEffect1(() => {
    if editable {
      returnFocus.current->Nullable.toOption->Option.forEach(element => restoreFocus(element, true))
      returnFocus.current = Nullable.null
    }
    None
  }, [editable])
  let workspace = workspaces->Array.find(w => w.id == creation.workspaceId)
  let available =
    workspace
    ->Option.map(w =>
      w.repos->Array.filter(r => !(draft.targets->Array.some(t => t.repoId == r.id)))
    )
    ->Option.getOr([])
  let send = action => {
    let _ = CreationRecovery.beforeAction(local.current, action, ~workspaces)
    switch local.current.snapshot {
    | Some(saved) => setDraft(_ => saved.creation.draft)
    | None => ()
    }
    dispatch(action)
  }
  let edit = value => send(EditCreationDraft({reviewId: creation.reviewId, edit: value}))
  let select = targetId => send(SelectCreationTarget({reviewId: creation.reviewId, targetId}))
  // A submitted recovery snapshot must contain the automatic defaults the
  // user saw. A newly added optimistic row may still be waiting for its ACK.
  let waitingDefaults =
    editable &&
    draft.targets->Array.some(t =>
      switch t.base {
      | Manual(_) => false
      | Automatic(_) =>
        switch creation.defaults->Array.find(d => d.repoId == t.repoId) {
        | Some({state: Ready(_) | Failed(_)}) => false
        | Some({state: Loading(_)}) | None => true
        }
      }
    )
  let retry = switch creation.status {
  | Interrupted(_) => true
  | Editing(_) | Failed(_) | Pending(_) | Reconciling(_) | Succeeded(_) => false
  }
  let retryReady = retry && !submitted
  React.useEffect1(() => {
    // A recovered interrupted form has disabled editors, so their autoFocus
    // cannot provide a native keyboard entry point. Offer the explicit retry
    // without activating it or taking focus away from another chosen control.
    if retryReady {
      submitRef.current->Nullable.toOption->Option.forEach(element => restoreFocus(element, false))
    }
    None
  }, [retryReady])
  let run = (command: View.Command.t) => {
    if command != Submit || (!submitted && (editable || retry) && !waitingDefaults) {
      if command == Submit {
        returnFocus.current = activeElement
        setSubmitted(_ => true)
      }
      send(Action.RunCommand({command: command}))
    }
  }
  let onKey = ev =>
    EditorKeys.handle(
      ~pending=pendingKeys.current,
      ~bindings,
      ~allowed=command =>
        switch command {
        | Submit | Back | AddReviewTarget | RemoveReviewTarget | ReconnectReviewCreation => true
        | _ => false
        },
      ~run,
      ev,
    )

  <form
    className="new-review panel"
    ariaLabel="new review"
    onKeyDown=onKey
    onSubmit={ev => {
      ReactEvent.Form.preventDefault(ev)
      run(Submit)
    }}
  >
    <p ariaLabel="Review workspace">
      {React.string(
        "Review in " ++
        workspace
        ->Option.map(w => RepositoryIdentity.workspaceLabel(workspaces, w))
        ->Option.getOr(RepositoryIdentity.shortId(creation.workspaceId)),
      )}
    </p>
    <UI.Field label="Review title">
      <UI.TextInput
        value=draft.title
        placeholder="Title"
        ariaLabel="Review title"
        autoFocus=true
        disabled={!editable}
        onChange={text => edit(Title({text: text}))}
        onKeyEvent=onKey
      />
    </UI.Field>
    {draft.targets
    ->Array.map(t => {
      let default =
        creation.defaults->Array.find(d => d.repoId == t.repoId)->Option.map(d => d.state)
      let base = switch t.base {
      | Manual({text}) => text
      | Automatic(_) =>
        switch default {
        | Some(Ready({base})) => RefSpecText.print(base)
        | Some(Loading(_) | Failed(_)) | None => ""
        }
      }
      <div key={Int.toString(t.id)} className="new-review-target">
        <UI.Field label="Repository">
          <UI.Select
            ariaLabel="repo"
            value=t.repoId
            disabled={!editable}
            options={
              let choices =
                workspace
                ->Option.map(w =>
                  w.repos
                  ->Array.filter(
                    r =>
                      r.id == t.repoId ||
                        !(
                          draft.targets->Array.some(
                            other => other.id != t.id && other.repoId == r.id,
                          )
                        ),
                  )
                  ->Array.map(r => (r.id, RepositoryIdentity.repoLabel(w, r.id)))
                )
                ->Option.getOr([])
              choices->Array.some(((id, _)) => id == t.repoId)
                ? choices
                : choices->Array.concat([
                    (t.repoId, RepositoryIdentity.shortId(t.repoId) ++ " (not attached)"),
                  ])
            }
            onFocus={() => select(t.id)}
            onKeyEvent=onKey
            onChange={repoId => edit(Repository({targetId: t.id, repoId}))}
          />
        </UI.Field>
        <UI.Field label="Base">
          <UI.TextInput
            value=base
            ariaLabel="Base revision"
            placeholder="Base revision"
            disabled={!editable}
            onFocus={() => select(t.id)}
            onKeyEvent=onKey
            onChange={text => edit(Base({targetId: t.id, text}))}
          />
        </UI.Field>
        <UI.Field label="Head">
          <UI.TextInput
            value=t.head
            ariaLabel="Head revision"
            placeholder="Head revision"
            disabled={!editable}
            onFocus={() => select(t.id)}
            onKeyEvent=onKey
            onChange={text => edit(Head({targetId: t.id, text}))}
          />
        </UI.Field>
        <UI.Button
          label="−"
          ariaLabel="Remove target"
          kind=Ghost
          disabled={!editable}
          title=?{Chrome.tip(chrome, RemoveReviewTarget)}
          onClick={() => {
            select(t.id)
            run(RemoveReviewTarget)
          }}
        />
        {switch (t.base, default) {
        | (Automatic(_), Some(Loading(_))) | (Automatic(_), None) =>
          <p role="status"> {React.string("Finding the default base…")} </p>
        | (Automatic(_), Some(Failed({message}))) =>
          <p role="alert">
            {React.string("Default base unavailable: " ++ message ++ ". Enter a base revision.")}
          </p>
        | (Automatic(_), Some(Ready(_))) | (Manual(_), _) => React.null
        }}
      </div>
    })
    ->React.array}
    {switch creation.status {
    | Editing(_) | Succeeded(_) => React.null
    | Failed({message}) | Interrupted({message}) =>
      <UI.Panel title="Unable to create review" role="alert">
        <p> {React.string(message)} </p>
      </UI.Panel>
    | Pending(_) =>
      <p role="status"> {React.string("Creating review… Your inputs are retained.")} </p>
    | Reconciling(_) =>
      <p role="status"> {React.string("Checking the previous creation attempt…")} </p>
    }}
    <UI.Box direction=Row gap=Sm>
      {switch connection {
      | Disconnected(_) | Rejected(_) =>
        <UI.Button
          label="Reconnect"
          title=?{Chrome.tip(chrome, ReconnectReviewCreation)}
          onClick={() => run(ReconnectReviewCreation)}
        />
      | Connecting(_) => <span role="status"> {React.string("Connecting…")} </span>
      | Subscribed(_) => React.null
      }}
      <UI.Button
        label="+ target"
        title=?{Chrome.tip(chrome, AddReviewTarget)}
        disabled={!editable || Array.length(available) == 0}
        onClick={() => run(AddReviewTarget)}
      />
      <UI.Button
        label={retry ? "Check and retry" : "Create"}
        buttonRef={ReactDOM.Ref.domRef(submitRef)}
        kind=Primary
        title=?{Chrome.tip(chrome, Submit)}
        disabled={submitted || !editable && !retry || waitingDefaults}
        onClick={() => run(Submit)}
      />
      <UI.Button
        label="Cancel"
        kind=Ghost
        title=?{Chrome.tip(chrome, Back)}
        disabled={!editable}
        onClick={() => run(Back)}
      />
    </UI.Box>
    {workspace->Option.map(w => Array.length(w.repos) == 0)->Option.getOr(true)
      ? <p className="new-review-hint">
          {React.string(
            "No repositories to review. Attach one to this workspace, then refresh (R).",
          )}
        </p>
      : Array.length(available) == 0
      ? <p className="new-review-hint">
        {React.string(
          "All workspace repositories are included. Remove a target to choose it again.",
        )}
      </p>
      : React.null}
  </form>
}
