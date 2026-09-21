// What the open review diffs, laid out exactly as the design canvas'
// Main artboard (UI-DESIGN §Layout): title · labelled Base → Head selectors ·
// `+ working tree` check · (right) compact diff settings · totals ·
// connection dot. A working-tree head shows the checked-out branch from
// the resolved targets. Menu hints come from the keymap chrome.

let focusMenuItem: (Nullable.t<Dom.element>, int) => unit = %raw(`(menu, index) => {
  menu?.querySelectorAll('[role="menuitemradio"]')[index]?.focus()
}`)
let focusSettingsButton: Nullable.t<Dom.element> => unit = %raw(`root => {
  root?.querySelector('[aria-controls="diff-settings-menu"]')?.focus()
}`)
let eventIsOutside: (Nullable.t<Dom.element>, 'event) => bool = %raw(`(root, event) => {
  return root == null || !root.contains(event.target)
}`)

module Pointer = {
  type event
  @val @scope("document") external listen: (string, event => unit) => unit = "addEventListener"
  @val @scope("document") external unlisten: (string, event => unit) => unit = "removeEventListener"
}

module Check = {
  /// The compact `+ working tree` scope chip.
  @react.component
  let make = (~label: string, ~checked: bool, ~title: option<string>, ~onToggle: unit => unit) => {
    let el =
      <label className="chip-check" ?title>
        <input type_="checkbox" checked onChange={_ => onToggle()} />
        {React.string(label)}
      </label>
    checked ? Attrs.withData(el, [("data-active", "true")]) : el
  }
}

@react.component
let make = (
  ~reviews: array<Domain.Review.t>,
  ~workspaces: array<Domain.Workspace.t>,
  ~resolvedTargets: array<Domain.ResolvedTarget.t>,
  ~openReview: option<Ids.reviewId>,
  ~daemonContext: option<View.DaemonContext.t>=?,
  ~activeRepo: option<Ids.repoId>=?,
  ~prefs: View.ViewPrefs.t,
  ~scope: Domain.DiffScope.t=Domain.DiffScope.All({}),
  ~chrome: array<View.Hint.t>=[],
  ~bindings: array<View.Hint.t>=[],
  ~connection: View.ConnectionView.t=View.ConnectionView.Disconnected({}),
  ~progress: View.Progress.t=View.ViewModel.empty.progress,
  ~refSelector: option<View.RefSelectorView.t>=?,
  ~dispatch: Action.t => unit=_ => (),
) => {
  let (settingsOpen, setSettingsOpen) = React.useState(() => false)
  let (settingsIndex, setSettingsIndex) = React.useState(() => 0)
  let settingsRef = React.useRef(Nullable.null)
  let menuRef = React.useRef(Nullable.null)
  let closeSettings = () => {
    setSettingsOpen(_ => false)
    focusSettingsButton(settingsRef.current)
  }
  let chooseSetting = index => {
    switch index {
    | 0 if prefs.layout != Unified => dispatch(SetLayout({layout: Unified}))
    | 1 if prefs.layout != Split => dispatch(SetLayout({layout: Split}))
    | 2 if prefs.ignoreWhitespace =>
      dispatch(SetRenderOpts({ignoreWhitespace: false, contextLines: prefs.contextLines}))
    | 3 if !prefs.ignoreWhitespace =>
      dispatch(SetRenderOpts({ignoreWhitespace: true, contextLines: prefs.contextLines}))
    | 0 | 1 | 2 | 3 | _ => ()
    }
    closeSettings()
  }
  let moveSetting = delta => {
    let next = mod(settingsIndex + delta + 4, 4)
    setSettingsIndex(_ => next)
    focusMenuItem(menuRef.current, next)
  }
  let menuKey = key =>
    switch key {
    | "ArrowDown" | "ArrowRight" => moveSetting(1)
    | "ArrowUp" | "ArrowLeft" => moveSetting(-1)
    | "Enter" | " " => chooseSetting(settingsIndex)
    | "Escape" => closeSettings()
    | "Tab" => setSettingsOpen(_ => false)
    | _ => ()
    }
  React.useEffect1(() => {
    if settingsOpen {
      let handler = event =>
        if eventIsOutside(settingsRef.current, event) {
          closeSettings()
        }
      // `click` runs after the pointer's focus default, so restoring the
      // trigger remains the final focus state even when the outside target
      // itself is focusable.
      Pointer.listen("click", handler)
      Some(() => Pointer.unlisten("click", handler))
    } else {
      None
    }
  }, [settingsOpen])

  switch openReview->Option.flatMap(id => reviews->Array.find(r => r.id == id)) {
  | None => React.null
  | Some(review) => {
      let workspace = workspaces->Array.find(w => w.id == review.workspaceId)
      let repoName = id =>
        workspace->Option.mapOr(RepositoryIdentity.shortId(id), w =>
          RepositoryIdentity.repoLabel(w, id)
        )
      let headText = (t: Domain.ReviewTarget.t) =>
        switch t.head {
        | WorkingTree(_) =>
          switch resolvedTargets->Array.find(r => r.repoId == t.repoId) {
          | Some({head: {source: WorkingTree({branch: Some(branch)})}}) => branch ++ " (worktree)"
          | Some(_) | None => RefSpecText.print(t.head)
          }
        | _ => RefSpecText.print(t.head)
        }
      let allish = switch scope {
      | All(_) | Committed(_) => true
      | Commit(_) | Worktree(_) | Requested(_) | SinceCheckpoint(_) => false
      }
      let conn = switch connection {
      | Disconnected(_) => "disconnected"
      | Connecting(_) => "connecting"
      | Subscribed(_) => "connected"
      | Rejected(_) => "rejected"
      }
      <header className="review-header" ariaLabel="review targets">
        <div className="review-identity" ariaLabel="Review location">
          <UI.Button
            label={"⌂ " ++
            workspace->Option.mapOr(
              "Workspace " ++ RepositoryIdentity.shortId(review.workspaceId),
              w => RepositoryIdentity.workspaceLabel(workspaces, w),
            )}
            kind=Ghost
            title=?{Chrome.tip(chrome, GoHome)}
            onClick={() => dispatch(GoHome({}))}
          />
          <RepositoryIdentity.Context context=daemonContext />
          {switch activeRepo {
          | Some(repoId) =>
            <span className="active-repository">
              {React.string("Active repository: " ++ repoName(repoId))}
            </span>
          | None =>
            <span className="active-repository">
              {React.string(
                Int.toString(Array.length(review.targets)) ++ " repositories in this review",
              )}
            </span>
          }}
        </div>
        <div className="review-header-main">
          <span className="review-header-title"> {React.string(review.title)} </span>
          {switch scope {
          | Requested({requestId}) =>
            <UI.Badge text={"Requested revision · request " ++ Float.toString(requestId)} />
          | SinceCheckpoint({checkpointId}) =>
            <UI.Badge text={"Changes since checkpoint " ++ Float.toString(checkpointId)} />
          | All(_) | Committed(_) | Commit(_) | Worktree(_) => React.null
          }}
          {switch scope {
          | Requested(_) | SinceCheckpoint(_) =>
            <UI.Button
              label="Back to all changes"
              title=?{Chrome.tip(chrome, ScopeAll)}
              onClick={() => dispatch(RunCommand({command: ScopeAll}))}
            />
          | All(_) | Committed(_) | Commit(_) | Worktree(_) => React.null
          }}
          {review.targets
          ->Array.map(t =>
            <span key=t.repoId className="review-header-target">
              <span className="review-header-repo">
                {React.string(repoName(t.repoId) ++ ":")}
              </span>
              <span className="review-header-side-label"> {React.string("Base")} </span>
              <UI.Button
                label={RefSpecText.print(t.base) ++ " ▾"}
                kind=Ghost
                onClick={() => dispatch(OpenRefSelector({repoId: t.repoId, side: Base}))}
              />
              <span className="review-header-arrow" ariaHidden=true> {React.string("→")} </span>
              <span className="review-header-side-label"> {React.string("Head")} </span>
              <UI.Button
                label={headText(t) ++ " ▾"}
                kind=Ghost
                onClick={() => dispatch(OpenRefSelector({repoId: t.repoId, side: Head}))}
              />
            </span>
          )
          ->React.array}
          {allish
            ? <Check
                label="+ working tree"
                checked={scope == All({})}
                title={Chrome.tip(chrome, ScopeWorktree)}
                onToggle={() =>
                  dispatch(SetScope({scope: scope == All({}) ? Committed({}) : All({})}))}
              />
            : React.null}
        </div>
        {switch scope {
        | Requested(_) | SinceCheckpoint(_) => <ReviewCheckpoints.Targets targets=resolvedTargets />
        | All(_) | Committed(_) | Commit(_) | Worktree(_) => React.null
        }}
        <div className="review-header-toggles">
          <div className="diff-settings" ref={ReactDOM.Ref.domRef(settingsRef)}>
            <UI.Button
              label="⚙"
              kind=Icon
              ariaLabel="Diff settings"
              ariaControls="diff-settings-menu"
              expanded=settingsOpen
              hasPopup=#menu
              onClick={() => {
                if settingsOpen {
                  closeSettings()
                } else {
                  setSettingsIndex(_ => prefs.layout == Unified ? 0 : 1)
                  setSettingsOpen(_ => true)
                }
              }}
            />
            {settingsOpen
              ? <div
                  id="diff-settings-menu"
                  className="diff-settings-menu"
                  role="menu"
                  ariaLabel="Diff settings menu"
                  ref={ReactDOM.Ref.domRef(menuRef)}
                >
                  <div className="menu-group" role="group" ariaLabel="Diff layout">
                    <span className="menu-group-label"> {React.string("Diff layout")} </span>
                    <UI.MenuItem
                      label="Unified"
                      checked={prefs.layout == Unified}
                      tabIndex={settingsIndex == 0 ? 0 : -1}
                      autoFocus={settingsIndex == 0}
                      hint=?{Chrome.keys(chrome, ToggleLayout)}
                      title=?{Chrome.tip(chrome, ToggleLayout)}
                      onFocus={() => setSettingsIndex(_ => 0)}
                      onKey=menuKey
                      onClick={() => chooseSetting(0)}
                    />
                    <UI.MenuItem
                      label="Split"
                      checked={prefs.layout == Split}
                      tabIndex={settingsIndex == 1 ? 0 : -1}
                      autoFocus={settingsIndex == 1}
                      hint=?{Chrome.keys(chrome, ToggleLayout)}
                      title=?{Chrome.tip(chrome, ToggleLayout)}
                      onFocus={() => setSettingsIndex(_ => 1)}
                      onKey=menuKey
                      onClick={() => chooseSetting(1)}
                    />
                  </div>
                  <div className="menu-group" role="group" ariaLabel="Whitespace changes">
                    <span className="menu-group-label"> {React.string("Whitespace changes")} </span>
                    <UI.MenuItem
                      label="Show"
                      checked={!prefs.ignoreWhitespace}
                      tabIndex={settingsIndex == 2 ? 0 : -1}
                      autoFocus={settingsIndex == 2}
                      hint=?{Chrome.keys(chrome, ToggleWhitespace)}
                      title=?{Chrome.tip(chrome, ToggleWhitespace)}
                      onFocus={() => setSettingsIndex(_ => 2)}
                      onKey=menuKey
                      onClick={() => chooseSetting(2)}
                    />
                    <UI.MenuItem
                      label="Hide"
                      checked=prefs.ignoreWhitespace
                      tabIndex={settingsIndex == 3 ? 0 : -1}
                      autoFocus={settingsIndex == 3}
                      hint=?{Chrome.keys(chrome, ToggleWhitespace)}
                      title=?{Chrome.tip(chrome, ToggleWhitespace)}
                      onFocus={() => setSettingsIndex(_ => 3)}
                      onKey=menuKey
                      onClick={() => chooseSetting(3)}
                    />
                  </div>
                </div>
              : React.null}
          </div>
          <span className="header-totals">
            {React.string(Int.toString(progress.total) ++ " files · ")}
            <span className="stat-add">
              {React.string("+" ++ Int.toString(progress.additions))}
            </span>
            {React.string(" ")}
            <span className="stat-del">
              {React.string("−" ++ Int.toString(progress.deletions))}
            </span>
          </span>
          <span className={"conn-dot conn-" ++ conn} title=conn />
        </div>
        {switch refSelector {
        | Some(selector) => <RefSelector selector bindings dispatch />
        | None => React.null
        }}
      </header>
    }
  }
}
