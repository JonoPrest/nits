// Thread list and the review conversation (§6.5).

open View

let openFindings = (threads: array<ThreadView.t>) =>
  threads->Array.filter(t => t.status == Open({}))->Array.length

let statusText = (status: ThreadStatus.t) =>
  switch status {
  | Open(_) => "open finding"
  | Resolved(_) => "resolved finding"
  | Informational(_) => "informational"
  | Deferred(_) => "deferred finding · unfixed"
  }

let authorName = (a: Domain.Author.t) =>
  switch a {
  | Human({name}) => name
  | Agent({name}) => name ++ " (agent)"
  | Daemon(_) => "nitsd"
  }

let placeText = (
  ~repositories: RepositoryIdentity.context=Unavailable,
  ~full=false,
  p: ThreadPlace.t,
) => {
  let fileText = full ? RepositoryIdentity.fileDescription : RepositoryIdentity.fileText
  switch p {
  | Review(_) => "review"
  | File({file}) => fileText(repositories, file)
  | Lines({file, start, end_}) =>
    fileText(repositories, file) ++
    ":" ++
    Int.toString(start) ++ (end_ > start ? "-" ++ Int.toString(end_) : "")
  }
}

let contextText = (context: Domain.CommentContext.t): string =>
  switch context {
  | Browse({reference}) => "browse @" ++ RefSpecText.print(reference)
  | Diff({change}) =>
    let short = oid => String.slice(oid, ~start=0, ~end=7)
    switch change {
    | Submodule({change}) => SubmoduleView.text(change)
    | Added({new}) => "added @" ++ short(new.oid)
    | Deleted({old}) => "deleted @" ++ short(old.oid)
    | Modified({old, new}) | Renamed({old, new}) => short(old.oid) ++ " → " ++ short(new.oid)
    }
  }

module Context = {
  @react.component
  let make = (~context: option<Domain.CommentContext.t>) =>
    switch context {
    | Some(context) => <UI.Badge text={contextText(context)} />
    | None => React.null
    }
}

let isResolved = (status: ThreadStatus.t) =>
  switch status {
  | Resolved(_) => true
  | _ => false
  }
let deferredFindings = (threads: array<ThreadView.t>) =>
  threads
  ->Array.filter(t =>
    switch t.status {
    | Deferred(_) => true
    | _ => false
    }
  )
  ->Array.length

module Disposition = {
  @react.component
  let make = (~status: ThreadStatus.t) =>
    switch status {
    | Deferred({reason, trackingUrl, by, at}) =>
      <UI.Box gap=Sm>
        <UI.Badge text="Deferred · unfixed" />
        <p> {React.string(reason)} </p>
        <span title={Stepper.absolute(at)}>
          {React.string("Recorded by " ++ authorName(by) ++ " · " ++ Stepper.relative(at))}
        </span>
        {switch trackingUrl {
        | Some(url) =>
          <div className="thread-body">
            <UI.Markdown.Link href=url>
              {React.string("External follow-up: " ++ url)}
            </UI.Markdown.Link>
          </div>
        | None => React.null
        }}
        <p>
          {React.string(
            "Outside current review scope. This does not approve the review or indicate deployment safety.",
          )}
        </p>
      </UI.Box>
    | Open(_) | Resolved(_) | Informational(_) => React.null
    }
}

module FindingActions = {
  @react.component
  let make = (~thread: ThreadView.t, ~chrome: array<Hint.t>, ~dispatch: Action.t => unit) =>
    switch thread.status {
    | Informational(_) => React.null
    | Open(_) =>
      <>
        <UI.Button
          label="Resolve finding"
          title=?{Chrome.tip(chrome, ToggleResolved)}
          onClick={() => dispatch(ResolveThread({threadId: thread.id}))}
        />
        <UI.Button
          label="Defer finding"
          title=?{Chrome.tip(chrome, DeferFinding)}
          onClick={() => dispatch(DeferOpened({threadId: thread.id}))}
        />
      </>
    | Resolved(_) | Deferred(_) =>
      <UI.Button
        label="Reopen finding"
        title=?{Chrome.tip(chrome, ToggleResolved)}
        onClick={() => dispatch(UnresolveThread({threadId: thread.id}))}
      />
    }
}

module Item = {
  @react.component
  let make = (
    ~thread: ThreadView.t,
    ~repositories: RepositoryIdentity.context=Unavailable,
    ~focused: bool,
    ~onSelect: unit => unit,
    ~onApply: unit => unit,
    ~onOriginal: unit => unit,
    ~chrome: array<Hint.t>,
    ~onReply: unit => unit,
    ~dispatch: Action.t => unit,
    ~composer: React.element,
    ~focusedComment: option<string>,
  ) => {
    let (focusRef, onKeyDown) = ThreadFocus.use(~focused)
    let flags =
      [
        isResolved(thread.status) ? "resolved" : "",
        thread.outdated ? "outdated" : "",
        thread.pending ? "pending" : "",
      ]->Array.filter(s => s != "")
    Attrs.focused(
      <li
        className={"thread-item " ++ flags->Array.join(" ")}
        ref={ReactDOM.Ref.domRef(focusRef)}
        tabIndex={focused ? 0 : -1}
        onKeyDown
        onClick={_ => onSelect()}
      >
        <div className="thread-meta">
          <span className="thread-author"> {React.string(authorName(thread.author))} </span>
          <UI.Badge text={statusText(thread.status)} />
          <span className="thread-place" title={placeText(~repositories, ~full=true, thread.place)}>
            {React.string(placeText(~repositories, thread.place))}
          </span>
          <Context context=thread.context />
          {thread.replies > 0
            ? <UI.Badge text={Int.toString(thread.replies) ++ " replies"} />
            : React.null}
          {thread.pending
            ? <span className="thread-pending"> {React.string("…")} </span>
            : React.null}
        </div>
        {focused || composer != React.null
          ? <ul className="thread-comments">
              {thread.comments
              ->Array.map(c =>
                <li
                  id={"comment-" ++ c.id}
                  key=c.id
                  className={"thread-comment" ++
                  (c.pending ? " pending" : "") ++ (
                    focusedComment == Some(c.id) ? " reference-target" : ""
                  )}
                >
                  <span onClick={ev => ReactEvent.Mouse.stopPropagation(ev)}>
                    <UI.CopyReference reference=c.reference chrome dispatch />
                  </span>
                  <div className="thread-meta">
                    <span className="thread-author"> {React.string(authorName(c.author))} </span>
                    <span title={Stepper.absolute(c.created)}>
                      {React.string(Stepper.relative(c.created))}
                    </span>
                    {c.pending
                      ? <span className="thread-pending"> {React.string("…")} </span>
                      : React.null}
                  </div>
                  <div className="thread-body">
                    <UI.Markdown source=c.body />
                  </div>
                </li>
              )
              ->React.array}
            </ul>
          : <div className="thread-summary"> {React.string(thread.summary)} </div>}
        <Disposition status=thread.status />
        <div onClick={ev => ReactEvent.Mouse.stopPropagation(ev)}>
          <UI.CopyReference reference=thread.reference chrome dispatch />
          {composer != React.null
            ? composer
            : <UI.Button label="Reply" title=?{Chrome.tip(chrome, Reply)} onClick=onReply />}
          <FindingActions thread chrome dispatch />
        </div>
        {thread.suggestion
          ? <UI.Button label="Apply suggestion (a)" kind=Primary onClick=onApply />
          : React.null}
        {switch (thread.outdated, thread.context) {
        | (true, Some(_)) =>
          <UI.Button label="Open original diff (enter)" kind=Ghost onClick=onOriginal />
        | (true, None) | (false, _) => React.null
        }}
      </li>,
      focused,
    )
  }
}

@react.component
let make = (
  ~title: string,
  ~repositories: RepositoryIdentity.context=Unavailable,
  ~threads: array<ThreadView.t>,
  ~focus: Focus.t,
  ~indexOffset: int,
  ~dispatch: Action.t => unit,
  ~chrome: array<Hint.t>=[],
  ~draft: option<Draft.t>=?,
  ~pendingRefresh: bool=false,
  ~focusedComment: option<string>=?,
) => {
  let focusedIndex = switch focus {
  | Thread({index}) => Some(index)
  | _ => None
  }
  <UI.Panel title>
    {Array.length(threads) == 0
      ? <UI.Empty text="No threads." />
      : <ul role="list">
          {threads
          ->Array.mapWithIndex((t, i) =>
            <Item
              key=t.id
              thread=t
              repositories
              dispatch
              focusedComment
              focused={focusedIndex == Some(indexOffset + i)}
              onSelect={() => {
                dispatch(SetFocus({focus: Focus.Thread({index: indexOffset + i})}))
                switch (t.outdated, t.context, t.place) {
                | (_, Some(Browse(_)), _) | (true, Some(Diff(_)), _) =>
                  dispatch(OpenOriginalDiff({threadId: t.id}))
                | (_, _, Lines({file, start})) =>
                  dispatch(
                    Viewport({
                      file,
                      firstRow: Int.toFloat(start - 30)->Math.max(0.)->Float.toInt,
                      lastRow: start + 30,
                    }),
                  )
                | (_, _, File({file})) => dispatch(Viewport({file, firstRow: 0, lastRow: 59}))
                | (_, _, Review(_)) => ()
                }
              }}
              chrome
              composer={switch draft {
              | Some(draft) if View.Draft.thread(draft) == Some(t.id) =>
                <Composer chrome draft pendingRefresh dispatch />
              | Some(_) | None => React.null
              }}
              onReply={() => dispatch(ReplyOpened({threadId: t.id}))}
              onApply={() => dispatch(ApplySuggestion({commentId: t.root}))}
              onOriginal={() => dispatch(OpenOriginalDiff({threadId: t.id}))}
            />
          )
          ->React.array}
        </ul>}
  </UI.Panel>
}
