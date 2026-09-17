// The UI is a renderer over `ViewModel` and a source of `Action`s
// (ARCHITECTURE §6.1). Keys outside text inputs go to the core as chords;
// text inputs stop propagation themselves.

@val @scope("window") external tauriInternals: Nullable.t<'a> = "__TAURI_INTERNALS__"

let chooseCore = (): Core.t =>
  switch tauriInternals->Nullable.toOption {
  | Some(_) => CoreTauri.make()
  | None => CoreWs.make(~url=CoreWs.defaultUrl())
  }

/// Keys the browser would otherwise act on when they reach the keymap.
let swallowed = ["Tab", " ", "ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", "End"]

module KeyEvent = {
  type t
  @get external key: t => string = "key"
  @get external ctrlKey: t => bool = "ctrlKey"
  @get external altKey: t => bool = "altKey"
  @get external shiftKey: t => bool = "shiftKey"
  @get external metaKey: t => bool = "metaKey"
  @get external target: t => Nullable.t<Dom.element> = "target"
  @get external tagName: Dom.element => string = "tagName"
  @send external preventDefault: t => unit = "preventDefault"
  @val @scope("window") external listen: (string, t => unit) => unit = "addEventListener"
  @val @scope("window") external unlisten: (string, t => unit) => unit = "removeEventListener"
}

/// The chord's text, as the keymap spells a binding (`y`, `ctrl+p`).
let chordText = (chord: Keys.KeyChord.t): string => {
  let key = switch chord.key {
  | Char({c}) => c == " " ? "space" : c
  | Named({key}) => Keys.NamedKeyName.of_(key)
  }
  (chord.mods.ctrl ? "ctrl+" : "") ++
  (chord.mods.alt ? "alt+" : "") ++
  (chord.mods.shift ? "shift+" : "") ++
  (chord.mods.meta ? "meta+" : "") ++
  key
}

/// The pending chord prefix, tracked in the shell rather than read back
/// from the model. The shell sends the chords, so it knows what has been
/// typed the instant it happens; `pendingKeys` in the view is the core's
/// answer to the *previous* key and arrives a round trip later, which is
/// exactly one keystroke too late to decide anything about this one.
module Pending = {
  type t = {mutable keys: array<string>}

  let make = (): t => {keys: []}

  /// What the core will make of `chord`, resolved against the same
  /// bindings it uses: a command, the start of a longer one, or nothing
  /// at all — which the core reports as an unbound key and does not
  /// count, so neither does the shell.
  type outcome =
    | Runs(View.Command.t)
    | Prefix
    | Unbound

  let step = (p: t, bindings: array<View.Hint.t>, chord: Keys.KeyChord.t): outcome => {
    let typed = Array.concat(p.keys, [chordText(chord)])
    let text = typed->Array.join(" ")
    let exact = bindings->Array.find(h => h.keys == text)
    let prefix = bindings->Array.some(h => h.keys->String.startsWith(text ++ " "))
    switch (exact, prefix) {
    | (Some(h), _) => {
        p.keys = []
        Runs(h.command)
      }
    | (None, true) => {
        p.keys = typed
        Prefix
      }
    | (None, false) => {
        // A key that cancels a pending sequence is still one the core
        // acts on; one typed with nothing pending is not.
        let cancelled = Array.length(p.keys) > 0
        p.keys = []
        cancelled ? Prefix : Unbound
      }
    }
  }
}

/// The bindings that apply where the focus is: the core's own applicable
/// set, aliases included. Not `hints` (primary bindings only, so `y` is
/// absent) and not `chrome` (one entry per command, no context, so `y`
/// would appear to be bound where it is not).
let bindingsFor = (model: View.ViewModel.t): array<View.Hint.t> => model.bindings

/// Keys outside text inputs become chords for the core; text inputs handle
/// their own keys and stop propagation.
let onKeyDown = (core: Core.t, ~onChord: Keys.KeyChord.t => unit, ev: KeyEvent.t) => {
  let key = KeyEvent.key(ev)
  let editing = switch KeyEvent.target(ev)->Nullable.toOption {
  | Some(el) => {
      let tag = KeyEvent.tagName(el)
      tag == "INPUT" || tag == "TEXTAREA"
    }
  | None => false
  }
  if !editing {
    switch Keys.ofBrowser({
      key,
      ctrlKey: KeyEvent.ctrlKey(ev),
      altKey: KeyEvent.altKey(ev),
      shiftKey: KeyEvent.shiftKey(ev),
      metaKey: KeyEvent.metaKey(ev),
    }) {
    | Some(chord) => {
        let search = key == "p" && (KeyEvent.ctrlKey(ev) || KeyEvent.metaKey(ev))
        // Printable chars are swallowed too: a chord that opens a text
        // input (`t`, `F`, `:`) must not also type itself into it once
        // the input autofocuses.
        let printable = String.length(key) == 1 && !KeyEvent.ctrlKey(ev) && !KeyEvent.metaKey(ev)
        if swallowed->Array.includes(key) || search || printable {
          KeyEvent.preventDefault(ev)
        }
        onChord(chord)
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
            ->Option.mapOr(false, command => command == CopyPath || command == CopyReference)
          ) {
            let target =
              model.lastKey->Option.flatMap(k => k.command) == Some(CopyReference)
                ? model.copyReference
                : model.copyTarget
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
            | Runs((CopyPath | CopyReference) as command) =>
              if seqOf(m) >= before {
                // The core has accounted for every key before this one,
                // so this view is the one this key acts on, and its
                // context is the one the key lands in: copy inside the
                // gesture, which is the only place the browser allows it.
                switch command == CopyReference ? m.copyReference : m.copyTarget {
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
      | _ => ()
      }
      core.dispatch(action)
    }
    let left = if Array.length(model.tree.roots) > 0 {
      let home =
        model.openReview
        ->Option.flatMap(id => model.reviews->Array.find(r => r.id == id))
        ->Option.flatMap(r => model.workspaces->Array.find(w => w.id == r.workspaceId))
        ->Option.map(w => w.name)
      <Tree tree=model.tree focus=model.focus ?home dispatch />
    } else {
      <ReviewList
        reviews=model.reviews
        workspaces=model.workspaces
        connection=model.connection
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
      <div className={"app-body" ++ (model.prefs.sidebarHidden ? " sidebar-hidden" : "")}>
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
            reviews=model.reviews
            workspaces=model.workspaces
            resolvedTargets=model.resolvedTargets
            openReview=model.openReview
            prefs=model.prefs
            scope=model.scope
            chrome=model.chrome
            connection=model.connection
            progress=model.progress
            refSelector=?model.refSelector
            dispatch
          />
          {switch model.lastError {
          | Some(Invalid({reason})) => <p role="alert"> {React.string(reason)} </p>
          | Some(NotFound({id})) =>
            <p role="alert"> {React.string("Reference target was not found: " ++ id)} </p>
          | _ => React.null
          }}
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
              | Some(search) => <SearchBox search dispatch />
              | None => React.null
              }}
              {switch model.diff {
              | Some(diff) if diff.original =>
                <DiffView
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
                <Composer chrome=model.chrome draft pendingRefresh=model.pendingRefresh dispatch />
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
                <Composer chrome=model.chrome draft pendingRefresh=model.pendingRefresh dispatch />
              | Some(_) | None => React.null
              }}
            </>
          | Browse =>
            <>
              <BrowseBar
                browseRef=model.browseRef
                repoId={model.resolvedTargets->Array.get(0)->Option.map(t => t.repoId)}
                dispatch
              />
              {switch model.tree.search {
              | Some(search) => <SearchBox search dispatch />
              | None => React.null
              }}
              {switch model.diff {
              | Some(diff) =>
                <DiffView
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
                <Composer chrome=model.chrome draft pendingRefresh=model.pendingRefresh dispatch />
              | Some(_) | None => React.null
              }}
            </>
          }}
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
      | Some(help) => <HelpOverlay help dispatch />
      | None => React.null
      }}
      {model.contentSearch != None || model.actionPalette
        ? <Palette
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
