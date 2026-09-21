// Editable text stays responsive locally; the core owns defaults, validation,
// pending writes, errors and reconciliation. Echoes never overwrite later edits.
@react.component
let make = (
  ~creation: View.ReviewCreation.t,
  ~workspaces: array<Domain.Workspace.t>,
  ~chrome: array<View.Hint.t>,
  ~bindings: array<View.Hint.t>,
  ~connection: View.ConnectionView.t=Subscribed({}),
  ~dispatch: Action.t => unit,
) => {
  let (draft, setDraft) = React.useState(() => creation.draft)
  let (submitted, setSubmitted) = React.useState(() => false)
  React.useEffect1(() => {
    switch creation.status {
    | Failed(_) | Interrupted(_) => setSubmitted(_ => false)
    | Editing(_) | Pending(_) | Reconciling(_) | Succeeded(_) => ()
    }
    None
  }, [creation.status])
  let draftRef = React.useRef(creation.draft)
  let sent = React.useRef([])
  let lastIncoming = React.useRef(creation.draft)
  let pendingKeys = React.useRef("")
  React.useEffect1(() => {
    let incoming = creation.draft
    if incoming != lastIncoming.current {
      lastIncoming.current = incoming
      let index = sent.current->Array.findIndex(value => value == incoming)
      if index >= 0 {
        sent.current = sent.current->Array.filterWithIndex((_, i) => i > index)
        if Array.length(sent.current) == 0 {
          draftRef.current = incoming
          setDraft(_ => incoming)
        }
      } else {
        sent.current = []
        draftRef.current = incoming
        setDraft(_ => incoming)
      }
    }
    None
  }, [creation.draft])
  let editable = View.ReviewCreation.editable(creation) && !submitted
  let workspace = workspaces->Array.find(w => w.id == creation.workspaceId)
  let update = next => {
    draftRef.current = next
    setDraft(_ => next)
    sent.current = sent.current->Array.concat([next])
    dispatch(UpdateCreationDraft({reviewId: creation.reviewId, draft: next}))
  }
  let target = (i, change) =>
    update({
      ...draftRef.current,
      targets: draftRef.current.targets->Array.mapWithIndex((t, j) => i == j ? change(t) : t),
    })
  let select = index => dispatch(SelectCreationTarget({reviewId: creation.reviewId, index}))
  let run = (command: View.Command.t) => {
    if command == Submit {
      setSubmitted(_ => true)
    }
    dispatch(Action.RunCommand({command: command}))
  }
  let onKey = (ev: ReactEvent.Keyboard.t) => {
    switch Keys.ofBrowser({
      key: ReactEvent.Keyboard.key(ev),
      ctrlKey: ReactEvent.Keyboard.ctrlKey(ev),
      altKey: ReactEvent.Keyboard.altKey(ev),
      shiftKey: ReactEvent.Keyboard.shiftKey(ev),
      metaKey: ReactEvent.Keyboard.metaKey(ev),
    }) {
    | None => ()
    | Some(chord) =>
      let text = (pendingKeys.current == "" ? "" : pendingKeys.current ++ " ") ++ Keys.text(chord)
      let allowed = bindings->Array.filter(h =>
        switch h.command {
        | Submit | Back | AddReviewTarget | RemoveReviewTarget | ReconnectReviewCreation => true
        | _ => false
        }
      )
      switch allowed->Array.find(h => h.keys == text) {
      | Some(hint) =>
        pendingKeys.current = ""
        ReactEvent.Keyboard.preventDefault(ev)
        run(hint.command)
      | None =>
        if allowed->Array.some(h => h.keys->String.startsWith(text ++ " ")) {
          pendingKeys.current = text
          ReactEvent.Keyboard.preventDefault(ev)
        } else {
          pendingKeys.current = ""
        }
      }
    }
    ReactEvent.Keyboard.stopPropagation(ev)
  }
  let retry = switch creation.status {
  | Interrupted(_) => true
  | Editing(_) | Failed(_) | Pending(_) | Reconciling(_) | Succeeded(_) => false
  }
  <form
    className="new-review panel"
    ariaLabel="new review"
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
        onChange={title => update({...draftRef.current, title})}
        onKeyEvent=onKey
      />
    </UI.Field>
    {draft.targets
    ->Array.mapWithIndex((t, i) => {
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
      <div key={Int.toString(i)} className="new-review-target">
        <UI.Field label="Repository">
          <UI.Select
            ariaLabel="repo"
            value=t.repoId
            disabled={!editable}
            options={workspace
            ->Option.map(w =>
              w.repos->Array.map(r => (r.id, RepositoryIdentity.repoLabel(w, r.id)))
            )
            ->Option.getOr([])}
            onFocus={() => select(i)}
            onKeyEvent=onKey
            onChange={repoId => target(i, t => {...t, repoId, base: Automatic({})})}
          />
        </UI.Field>
        <UI.Field label="Base">
          <UI.TextInput
            value=base
            ariaLabel="Base revision"
            placeholder="Base revision"
            disabled={!editable}
            onFocus={() => select(i)}
            onKeyEvent=onKey
            onChange={text => target(i, t => {...t, base: Manual({text: text})})}
          />
        </UI.Field>
        <UI.Field label="Head">
          <UI.TextInput
            value=t.head
            ariaLabel="Head revision"
            placeholder="Head revision"
            disabled={!editable}
            onFocus={() => select(i)}
            onKeyEvent=onKey
            onChange={head => target(i, t => {...t, head})}
          />
        </UI.Field>
        <UI.Button
          label="−"
          ariaLabel="Remove target"
          kind=Ghost
          disabled={!editable}
          title=?{Chrome.tip(chrome, RemoveReviewTarget)}
          onClick={() => {
            select(i)
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
        disabled={!editable ||
        workspace->Option.map(w => Array.length(w.repos) == 0)->Option.getOr(true)}
        onClick={() => run(AddReviewTarget)}
      />
      <UI.Button
        label={retry ? "Check and retry" : "Create"}
        kind=Primary
        title=?{Chrome.tip(chrome, Submit)}
        disabled={!editable && !retry}
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
      : React.null}
  </form>
}
