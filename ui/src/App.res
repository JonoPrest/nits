// The UI is a renderer over `ViewModel` and a source of `Action`s
// (ARCHITECTURE §6.1). Keys outside text inputs go to the core as chords;
// text inputs stop propagation themselves.

@val @scope("window") external tauriInternals: Nullable.t<'a> = "__TAURI_INTERNALS__"

let chooseCore = (): Core.t =>
  switch tauriInternals->Nullable.toOption {
  | Some(_) => CoreTauri.make()
  | None => CoreWs.make(~url=CoreWs.defaultUrl())
  }

module KeyEvent = {
  type t
  @get external key: t => string = "key"
  @get external ctrlKey: t => bool = "ctrlKey"
  @get external altKey: t => bool = "altKey"
  @get external shiftKey: t => bool = "shiftKey"
  @get external metaKey: t => bool = "metaKey"
  @get external target: t => Nullable.t<Dom.element> = "target"
  @send external preventDefault: t => unit = "preventDefault"
  @val @scope("window") external listen: (string, t => unit) => unit = "addEventListener"
  @val @scope("window") external unlisten: (string, t => unit) => unit = "removeEventListener"
}

/// The chord's text, as the keymap spells a binding (`y`, `ctrl+p`).
let chordText = Keys.text

/// The pending chord prefix, tracked in the shell rather than read back
/// from the model. The shell sends the chords, so it knows what has been
/// typed the instant it happens; `pendingKeys` in the view is the core's
/// answer to the *previous* key and arrives a round trip later, which is
/// exactly one keystroke too late to decide anything about this one.
module Pending = KeySequence

/// The bindings that apply where the focus is: the core's own applicable
/// set, aliases included. Not `hints` (primary bindings only, so `y` is
/// absent) and not `chrome` (one entry per command, no context, so `y`
/// would appear to be bound where it is not).
let bindingsFor = (model: View.ViewModel.t): array<View.Hint.t> => model.bindings

/// Keys outside text inputs become chords for the core; text inputs handle
/// their own keys and stop propagation.
// Native editing and activation stay in the browser. A checkbox is not a
// text editor: other chords still reach the shared keymap after a click.
let nativeKey: KeyEvent.t => bool = %raw(`event => {
  const target = event.target;
  if (event.defaultPrevented || event.isComposing || event.keyCode === 229) return true;
  if (!target) return false;
  if (target.isContentEditable || target.closest?.('[contenteditable="true"]')) return true;
  if (target.tagName === 'TEXTAREA' || target.tagName === 'SELECT') return true;
  if (target.tagName === 'INPUT') {
    if (!['checkbox', 'radio', 'button', 'submit', 'reset'].includes(target.type)) return true;
    if (event.key === ' ' || event.key === 'Enter') return true;
    if (target.type === 'radio' && event.key.startsWith('Arrow')) return true;
  }
  return (target.tagName === 'BUTTON' || (target.tagName === 'A' && target.hasAttribute('href')))
    && (event.key === 'Enter' || event.key === ' ');
}`)

let onKeyDown = (core: Core.t, ~onChord: Keys.KeyChord.t => Pending.outcome, ev: KeyEvent.t) => {
  if !nativeKey(ev) {
    switch Keys.ofBrowser({
      key: KeyEvent.key(ev),
      ctrlKey: KeyEvent.ctrlKey(ev),
      altKey: KeyEvent.altKey(ev),
      shiftKey: KeyEvent.shiftKey(ev),
      metaKey: KeyEvent.metaKey(ev),
    }) {
    | Some(chord) => {
        // Use the same single resolution as gesture-timed copying. Unbound
        // browser shortcuts keep their native behavior; sent-key accounting
        // still includes every chord delivered to the core.
        switch onChord(chord) {
        | Runs(_) | Prefix => KeyEvent.preventDefault(ev)
        | Unbound => ()
        }
        core.key(chord)
      }
    | None => ()
    }
  }
}

module Shell = {
  @react.component
  let make = (~core: Core.t) => {
    let (model, setModel) = React.useState(() => View.ViewModel.empty)
    let repositories = RepositoryIdentity.ofView(model)
    // A core Realign preserves the logical line/side while expansion
    // renumbers its row. Snapshot the browser geometry before React sees
    // each patch, then put that same identity back at the same pixel in a
    // layout effect. Future hosts can keep consuming the core's typed
    // focus/viewport result without inheriting browser DOM mechanics.
    let pendingAnchor = React.useRef(None)
    let anchorRestored = React.useRef(false)
    React.useEffect0(() => {
      let unsubscribe = core.subscribe(m => {
        switch Scroll.captureAnchor() {
        | Some(anchor) => pendingAnchor.current = Some(anchor)
        // An uncached-render patch temporarily removes the row. Keep the
        // last geometry until the replacement render can consume it.
        | None => ()
        }
        setModel(_ => m)
      })
      core.attach()
      Some(unsubscribe)
    })
    React.useLayoutEffect1(() => {
      anchorRestored.current =
        pendingAnchor.current->Option.mapOr(false, anchor => {
          let restored = Scroll.restoreAnchor(anchor)
          if restored {
            pendingAnchor.current = None
          }
          restored
        })
      None
    }, [model])
    // Deep link: open `?review=<id>` once the daemon is subscribed.
    let deepLinked = React.useRef(false)
    React.useEffect1(() => {
      switch (model.connection, CoreWs.referenceParam()->Option.orElse(CoreWs.reviewParam())) {
      | (Disconnected(_), _) => deepLinked.current = false
      | (Subscribed(_), Some(reviewId)) if !deepLinked.current => {
          deepLinked.current = true
          switch CoreWs.referenceParam() {
          | Some(reference) => core.dispatch(OpenReference({reference: reference}))
          | None => core.dispatch(OpenReview({reviewId: reviewId}))
          }
        }
      | _ => ()
      }
      None
    }, [model.connection])
    // Copying happens here, in the gesture that asks for it: a clipboard
    // write needs transient user activation, which a round trip through
    // the core spends. The core still decides WHICH file —
    // `model.copyTarget` — and each browser socket owns that core.
    let (toast, setToast) = React.useState(() => None)
    // One writer for the shell, so a slow write that settles after a
    // later one cannot overwrite what the reader is now looking at.
    let writer = React.useRef(Clipboard.latest())
    let copy = (path: string) =>
      writer.current(path, (text, failed) => setToast(_ => Some((text, failed))))
    let modelRef = React.useRef(model)
    modelRef.current = model
    let pending = React.useRef(Pending.make())
    // Keys this shell has sent, counted against the core's own count of
    // keys acted on. The core applies keys in order, so `j` then `y`
    // copies the file `j` moved to; a target read before the core has
    // accounted for `j` is the previous file. A model is not an
    // acknowledgement — a command that changes nothing emits no patch of
    // its own — so the correlation is the count, not the arrival.
    let sent = React.useRef(0)
    // Keys waiting on the core's verdict, by their number. A key typed
    // before the core has answered the one before it may land somewhere
    // else entirely (`g t` moves to the threads, where `y` is bound to
    // nothing), so what it copies — and whether it copies at all — is
    // the core's to say, not this shell's to predict.
    let awaitingCopy = React.useRef([])
    let seqOf = (m: View.ViewModel.t) => m.lastKey->Option.mapOr(0, k => k.seq)
    React.useEffect1(() => {
      let seq = seqOf(model)

      switch (model.connection, model.lastKey) {
      | (Disconnected(_), None) => {
          // CoreWs reconnects to a fresh host/core. Forget every verdict
          // belonging to the ended session before its sequence restarts.
          sent.current = 0
          awaitingCopy.current = []
          pending.current.keys = []
        }
      | _ => {
          // Nothing of ours outstanding: adopt the core's count. A UI can
          // still remount against the same Tauri/core session, so where it
          // has got to is never this shell's to assume.
          if seq >= sent.current {
            sent.current = seq
          }
          let answered = awaitingCopy.current->Array.some(wanted => wanted == seq)

          // A verdict this shell never saw (two keys inside one batch)
          // drops the copy rather than guessing at it.
          awaitingCopy.current = awaitingCopy.current->Array.filter(wanted => wanted > seq)
          if (
            answered &&
            model.lastKey
            ->Option.flatMap(k => k.command)
            ->Option.mapOr(false, command =>
              command == CopyPath || command == CopyReference || command == CopyCheckout
            )
          ) {
            let target = switch model.lastKey->Option.flatMap(k => k.command) {
            | Some(CopyReference) => model.copyReference
            | Some(CopyCheckout) => model.copyCheckout
            | _ => model.copyTarget
            }
            switch target {
            | Some(path) => copy(path)
            | None => ()
            }
          }
        }
      }
      None
    }, [model])
    React.useEffect0(() => {
      let handler = ev =>
        onKeyDown(
          core,
          ~onChord=chord => {
            let m = modelRef.current
            // The keydown IS the gesture, so the copy happens now — from
            // the prefix this shell has typed (the core's `pendingKeys`
            // is its answer to the previous key, a round trip behind) and
            // from the bindings that actually apply where the focus is.
            let outcome = Pending.step(pending.current, bindingsFor(m), chord)
            // Every chord this shell hands to the core is counted, and
            // the core counts every one it is handed — whatever it makes
            // of them. Counting only the keys that resolve would need
            // this shell to predict which ones do, which is the thing it
            // cannot do: an unbound key, or one whose command has no
            // target here, is rejected there and counted all the same.
            let before = sent.current
            sent.current = before + 1
            switch outcome {
            | Runs((CopyPath | CopyReference | CopyCheckout) as command) =>
              if seqOf(m) >= before {
                // The core has accounted for every key before this one,
                // so this view is the one this key acts on, and its
                // context is the one the key lands in: copy inside the
                // gesture, which is the only place the browser allows it.
                let target = switch command {
                | CopyReference => m.copyReference
                | CopyCheckout => m.copyCheckout
                | _ => m.copyTarget
                }
                switch target {
                | Some(path) => copy(path)
                | None => ()
                }
              } else {
                // Earlier keys are still unaccounted for. Where they
                // leave the focus decides what this key means, so wait
                // for the core to say — and copy nothing if it says the
                // key meant nothing there.
                awaitingCopy.current->Array.push(before + 1)
              }
            | Runs(_) | Prefix | Unbound => ()
            }
            outcome
          },
          ev,
        )
      KeyEvent.listen("keydown", handler)
      Some(() => KeyEvent.unlisten("keydown", handler))
    })
    // Keep whatever is focused on screen. On the diff that means vim-style
    // edge scrolling: the row moves, the view follows by as little as it
    // can, and `z z`/`z t`/`z b` reposition it outright.
    //
    // The trigger has to cover every way the row can end up in the wrong
    // place: the focus moving, the row arriving (a jump like `G` focuses a
    // row whose chunk is still in flight, so there is nothing to scroll to
    // on the first render), a new reposition, and the tab remounting the
    // scroller with a fresh scroll position.
    let focusedRow = switch model.focus {
    | Diff({row}) => Some(row)
    | ReviewRequest(_)
    | ReviewList(_)
    | Tree(_)
    | Thread(_)
    | CommitStepper(_)
    | Composer(_)
    | Help(_) =>
      None
    }
    let rowPresent = switch (focusedRow, model.diff) {
    | (Some(row), Some(d)) => d.rows->Array.some((r: View.DiffRow.t) => r.index == row)
    | (Some(_), None) | (None, _) => false
    }
    // An intent that already existed when this shell mounted has been
    // performed by whoever was showing the row before.
    let seen = React.useRef(model.scroll->Option.map(s => s.seq))
    let file = switch model.diff {
    | Some(d) => d.file.repoId ++ ":" ++ d.file.path
    | None => ""
    }
    let key =
      [
        JSON.stringifyAny(model.focus)->Option.getOr(""),
        file,
        rowPresent ? "1" : "0",
        model.scroll->Option.map(s => Int.toString(s.seq))->Option.getOr(""),
        JSON.stringifyAny(model.tab)->Option.getOr(""),
      ]->Array.join("|")
    React.useEffect1(() => {
      let (step, next) = Scroll.plan(
        ~focus=model.focus,
        ~scroll=model.scroll,
        ~present=rowPresent,
        ~seen=seen.current,
      )
      seen.current = next
      switch step {
      | Skip => ()
      // The exact pixel anchor is stronger than ordinary scrolloff
      // following for the patch that performed a core Realign.
      | Follow if anchorRestored.current => ()
      | Follow => Scroll.apply(Nearest)
      | Reposition(align) => Scroll.apply(Align(align))
      | List => Focused.scrollIntoView()
      }
      anchorRestored.current = false
      None
    }, [key])
    React.useEffect1(() => {
      if model.tab == Conversation {
        model.focusedComment->Option.forEach(Focused.comment)
      }
      None
    }, [key ++ model.focusedComment->Option.getOr("")])
    let dispatch = (action: Action.t) => {
      switch action {
      // Inside the click, for the same reason as the key press above.
      | CopyPath({path}) => copy(path)
      | CopyReference({reference}) => copy(reference)
      | CopyCheckout({repoId}) =>
        model.workspaces
        ->Array.findMap(w => w.repos->Array.find(r => r.id == repoId))
        ->Option.forEach(repo => copy(repo.path))
      | _ => ()
      }
      core.dispatch(action)
    }
    let left = if model.openReview != None {
      let home =
        model.openReview
        ->Option.flatMap(id => model.reviews->Array.find(r => r.id == id))
        ->Option.flatMap(r => model.workspaces->Array.find(w => w.id == r.workspaceId))
        ->Option.map(w => w.name)
      <Tree tree=model.tree focus=model.focus ?home chrome=model.chrome dispatch />
    } else {
      <ReviewList
        reviews=model.reviews
        workspaces=model.workspaces
        home=model.home
        chrome=model.chrome
        focus=model.focus
        dispatch
      />
    }
    // The sidebar auto-expands to fit full file names while the tree is
    // focused; otherwise names truncate at the resting width.
    let treeFocused = switch model.focus {
    | Tree(_) => true
    | _ => false
    }
    let sidebarAction = model.prefs.sidebarHidden ? "Show file tree" : "Hide file tree"
    <main className="app-shell">
      <div
        className={"app-body" ++
        (model.openReview == None ? " app-home" : "") ++ (
          model.prefs.sidebarHidden ? " sidebar-hidden" : ""
        )}
      >
        <div className="sidebar-toggle">
          <UI.Button
            label={model.prefs.sidebarHidden ? "◨" : "◧"}
            kind=Icon
            ariaLabel=sidebarAction
            ariaControls="file-tree-sidebar"
            expanded={!model.prefs.sidebarHidden}
            title=?{Chrome.tip(model.chrome, ToggleSidebar)}
            onClick={() => dispatch(ToggleSidebar({}))}
          />
        </div>
        {model.prefs.sidebarHidden
          ? React.null
          : <aside
              id="file-tree-sidebar"
              className={"app-left" ++ (treeFocused ? " app-left-expanded" : "")}
            >
              <div className="app-left-tree"> left </div>
              {switch model.stepper {
              | Some(stepper) => <Stepper stepper scope=model.scope focus=model.focus dispatch />
              | None => React.null
              }}
            </aside>}
        <div className="app-center">
          <ReviewHeader
            bindings=model.bindings
            reviews=model.reviews
            workspaces=model.workspaces
            resolvedTargets=model.resolvedTargets
            openReview=model.openReview
            daemonContext=?model.daemonContext
            activeRepo=?model.activeRepo
            prefs=model.prefs
            scope=model.scope
            chrome=model.chrome
            connection=model.connection
            progress=model.progress
            refSelector=?model.refSelector
            dispatch
          />
          {switch model.lastError {
          | Some(error) => <p role="alert"> {React.string(RpcErrorText.message(error))} </p>
          | None => React.null
          }}
          {model.openReview == None
            ? <WorkspaceHome model dispatch />
            : <>
                <Tabs
                  tab=model.tab
                  fileCount=model.progress.total
                  threadCount={Threads.openFindings(model.threads)}
                  requestCount={Array.length(model.requests)}
                  deferredCount={Threads.deferredFindings(model.threads)}
                  chrome=model.chrome
                  dispatch
                />
                {switch model.tab {
                | FilesChanged =>
                  <>
                    {switch model.tree.search {
                    | Some(search) =>
                      <SearchBox bindings=model.bindings search repositories dispatch />
                    | None => React.null
                    }}
                    {switch model.diff {
                    | Some(diff) if diff.original =>
                      <DiffView
                        bindings=model.bindings
                        repositories
                        diff
                        layout=model.prefs.layout
                        focus=model.focus
                        scroll=?model.scroll
                        chrome=model.chrome
                        threads=model.threads
                        dispatch
                      />
                    | _ =>
                      Array.length(model.diffs) > 0
                        ? <div className="diff-stack">
                            {model.diffs
                            ->Array.map(diff =>
                              <FileDiff
                                bindings=model.bindings
                                repositories
                                key={diff.file.repoId ++ diff.file.path}
                                diff
                                layout=model.prefs.layout
                                focus=model.focus
                                threads=model.threads
                                draft=model.draft
                                pendingRefresh=model.pendingRefresh
                                chrome=model.chrome
                                visual=?model.visual
                                isOpen={switch model.diff {
                                | Some(open_) => open_.file == diff.file
                                | None => false
                                }}
                                dispatch
                              />
                            )
                            ->React.array}
                          </div>
                        : <div className="diff-empty"> {React.string("No changed files")} </div>
                    }}
                    {switch model.draft {
                    | Some(draft) if View.Draft.isDocked(draft) =>
                      <Composer
                        bindings=model.bindings
                        chrome=model.chrome
                        draft
                        pendingRefresh=model.pendingRefresh
                        dispatch
                      />
                    | Some(_) | None => React.null
                    }}
                  </>
                | Conversation =>
                  // Every thread of the review, chronologically (GitHub-style):
                  // file/line threads and review-level ones together.
                  <>
                    <UI.Box>
                      <UI.Button
                        label="Add informational note"
                        title=?{Chrome.tip(model.chrome, InformationalNote)}
                        onClick={() => dispatch(RunCommand({command: InformationalNote}))}
                      />
                      <UI.Button
                        label="Add review-wide finding"
                        title=?{Chrome.tip(model.chrome, ReviewFinding)}
                        onClick={() => dispatch(RunCommand({command: ReviewFinding}))}
                      />
                    </UI.Box>
                    <ReviewCheckpoints
                      checkpoints=model.checkpoints
                      checkCurrentReady=model.checkCurrentReady
                      chrome=model.chrome
                      dispatch
                    />
                    <ReviewRequests
                      requests=model.requests focus=model.focus chrome=model.chrome dispatch
                    />
                    <UI.Box direction=Row gap=Sm>
                      <UI.Badge
                        text={Int.toString(Threads.openFindings(model.threads)) ++ " open findings"}
                      />
                      <UI.Badge
                        text={Int.toString(
                          Threads.deferredFindings(model.threads),
                        ) ++ " deferred · unfixed"}
                      />
                    </UI.Box>
                    <Threads
                      bindings=model.bindings
                      repositories
                      title="Conversation"
                      focusedComment=?model.focusedComment
                      threads=model.threads
                      focus=model.focus
                      indexOffset=0
                      dispatch
                      chrome=model.chrome
                      draft=?model.draft
                      pendingRefresh=model.pendingRefresh
                    />
                    {switch model.draft {
                    | Some(draft) if View.Draft.thread(draft) == None =>
                      <Composer
                        bindings=model.bindings
                        chrome=model.chrome
                        draft
                        pendingRefresh=model.pendingRefresh
                        dispatch
                      />
                    | Some(_) | None => React.null
                    }}
                  </>
                | Browse =>
                  <>
                    {switch model.browse {
                    | Some(browse) =>
                      <BrowseBar
                        browse
                        targets={model.openReview
                        ->Option.flatMap(id => model.reviews->Array.find(review => review.id == id))
                        ->Option.mapOr([], review => review.targets)}
                        repositories
                        chrome=model.chrome
                        disabled={model.draft != None}
                        dispatch
                      />
                    | None => React.null
                    }}
                    {switch model.tree.search {
                    | Some(search) =>
                      <SearchBox bindings=model.bindings search repositories dispatch />
                    | None => React.null
                    }}
                    {switch model.diff {
                    | Some(diff) =>
                      <DiffView
                        bindings=model.bindings
                        repositories
                        diff
                        layout=model.prefs.layout
                        visual=?model.visual
                        focus=model.focus
                        scroll=?model.scroll
                        chrome=model.chrome
                        threads=model.threads
                        draft=?model.draft
                        pendingRefresh=model.pendingRefresh
                        dispatch
                      />
                    | None => <div className="diff-empty"> {React.string("Open a file")} </div>
                    }}
                    {switch model.draft {
                    // Browse composes inline too; only a draft with no row of
                    // its own docks here.
                    | Some(draft) if View.Draft.isDocked(draft) =>
                      <Composer
                        bindings=model.bindings
                        chrome=model.chrome
                        draft
                        pendingRefresh=model.pendingRefresh
                        dispatch
                      />
                    | Some(_) | None => React.null
                    }}
                  </>
                }}
              </>}
        </div>
      </div>
      <Toast message=toast />
      <WhichKey pendingKeys=model.pendingKeys pendingLabel=model.pendingLabel hints=model.hints />
      <HintBar
        hints=model.hints
        pendingKeys=model.pendingKeys
        pendingLabel=?model.pendingLabel
        mode=model.mode
        leader=model.leader
        focusName={switch model.focus {
        | Tree(_) => "TREE"
        | Diff(_) => "DIFF"
        | Thread(_) => "THREAD"
        | ReviewRequest(_) => "REQUESTS"
        | ReviewList(_) => "REVIEWS"
        | CommitStepper(_) => "COMMITS"
        | Composer(_) | Help(_) => ""
        }}
        connection=model.connection
        progress=model.progress
      />
      {switch model.help {
      | Some(help) => <HelpOverlay bindings=model.bindings help dispatch />
      | None => React.null
      }}
      {model.contentSearch != None || model.actionPalette
        ? <Palette
            bindings=model.bindings
            repositories
            contentSearch=model.contentSearch
            actionPalette=model.actionPalette
            chrome=model.chrome
            dispatch
          />
        : React.null}
    </main>
  }
}

@react.component
let make = () => {
  let core = React.useMemo0(chooseCore)
  <Shell core />
}
