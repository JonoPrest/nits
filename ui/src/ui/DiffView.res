// The open file (§6.5): a virtualized list over `total_rows`; rows the
// cache holds render, the rest are placeholders until their chunk lands.
// Scrolling dispatches `Viewport` so the core fetches what is visible.
// Threads render inline under their anchored row (UI-DESIGN §Comments);
// rows with threads are measured dynamically.

open View

let rowHeight = 20

/// Rows the virtualizer shows → the viewport the core should serve.
let viewportOf = (items: array<Virtual.virtualItem>): option<(int, int)> =>
  switch (items[0], items[Array.length(items) - 1]) {
  | (Some(first), Some(last)) => Some((first.index, last.index))
  | _ => None
  }

@react.component
let make = (
  ~repositories: RepositoryIdentity.context=Unavailable,
  ~diff: DiffView.t,
  ~layout: Layout.t,
  ~focus: Focus.t,
  ~visual: option<VisualView.t>=?,
  ~scroll: option<ScrollIntent.t>=?,
  ~chrome: array<View.Hint.t>=[],
  ~bindings: array<View.Hint.t>=[],
  ~threads: array<ThreadView.t>=[],
  ~draft: option<Draft.t>=?,
  ~pendingRefresh: bool=false,
  ~dispatch: Action.t => unit,
) => {
  // Plain blobs have one semantic side even when the saved diff layout is split.
  let layout = switch diff.target {
  | Blob(_) => View.Layout.Unified
  | Diff(_) => layout
  }
  let scrollRef = React.useRef(Nullable.null)
  let (drag, setDrag) = React.useState(() => None)
  let total = switch diff.content {
  | Text({totalRows}) => totalRows
  | Binary(_) | Submodule(_) => 0
  }
  let virtualizer = Virtual.useVirtualizer({
    count: total,
    getScrollElement: () => scrollRef.current,
    estimateSize: _ => rowHeight,
    overscan: 10,
  })
  let items = virtualizer->Virtual.getVirtualItems
  let viewport = viewportOf(items)
  // Ask for the window we show whenever it moves off what the core holds.
  React.useEffect2(() => {
    switch viewport {
    | Some((first, last)) if first != diff.firstRow || last != diff.lastRow =>
      dispatch(Viewport({file: diff.file, firstRow: first, lastRow: last}))
    | _ => ()
    }
    None
  }, (viewport, diff.file))
  // Keep the focused row on screen.
  let focusedRow = switch focus {
  | Diff({row}) => Some(row)
  | _ => None
  }
  let focusedSide = switch focus {
  | Diff({side}) => side
  | _ => Domain.Side.Head
  }
  React.useEffect1(() => {
    switch focusedRow {
    | Some(row) => virtualizer->Virtual.scrollToIndexAligned(row, {"align": "auto"})
    | None => ()
    }
    None
  }, [focusedRow])
  // `z z`/`z t`/`z b` reposition the view around the focused row; the
  // core counts the instructions so the same chord twice scrolls twice.
  // The watermark starts at whatever intent exists when this view mounts:
  // switching tabs away and back must not replay an old reposition as if
  // the reader had just asked for it.
  let lastScroll = React.useRef(scroll->Option.map((s: ScrollIntent.t) => s.seq))
  React.useEffect1(() => {
    switch scroll {
    | Some({seq, row, align}) if lastScroll.current != Some(seq) => {
        lastScroll.current = Some(seq)
        let align = switch align {
        | View.ScrollAlign.Center => "center"
        | Top => "start"
        | Bottom => "end"
        }
        virtualizer->Virtual.scrollToIndexAligned(row, {"align": align})
      }
    | Some(_) | None => ()
    }
    None
  }, [scroll])
  // Browse and stacked files use the same core-owned fold command.
  let collapsed = diff.collapsed
  let key = DiffSeen.fileKey(diff)
  let prevKey = React.useRef("")
  let seen = React.useRef(Dict.make())
  seen.current = DiffSeen.mergeSeen(seen.current, prevKey.current, key, diff.rows)
  prevKey.current = key
  let cached = seen.current
  let threadOf = (id: Ids.threadId) => threads->Array.findIndexOpt(t => t.id == id)
  let focusedThread = switch focus {
  | Thread({index}) => Some(index)
  | _ => None
  }
  let replyTo = draft->Option.flatMap(d => View.Draft.thread(d))
  let title = RepositoryIdentity.fileText(repositories, diff.file)
  let stats = switch diff.content {
  | Text({additions, deletions}) =>
    <span className="file-stats">
      <span className="stat-add"> {React.string("+" ++ Int.toString(additions))} </span>
      <span className="stat-del"> {React.string("−" ++ Int.toString(deletions))} </span>
    </span>
  | Binary(_) | Submodule(_) => React.null
  }
  let binary = switch diff.content {
  | Submodule(_) => <SubmoduleView target=diff.target />
  | Binary(_) => <div className="diff-binary"> {React.string("binary file")} </div>
  | Text(_) => React.null
  }
  <section className="diff-panel panel" role="grid" ariaLabel=title>
    <header className="panel-header file-header">
      <RepositoryIdentity.File repositories file=diff.file />
      <UI.CopyPath path={diff.file.path} fileLabel=title chrome dispatch />
      stats
      <BlobMetadata target=diff.target />
      {switch (diff.target, diff.content) {
      | (Diff(_), Text(_)) =>
        <UI.Button
          label="expand file"
          kind=Ghost
          title=?{Chrome.tip(chrome, ExpandFile)}
          ariaLabel={"Expand context for " ++ title}
          onClick={() => dispatch(ExpandContext({file: diff.file, full: true}))}
        />
      | (Blob(_), _) | (Diff(_), Binary(_) | Submodule(_)) => React.null
      }}
    </header>
    {diff.original
      ? <div className="original-banner" role="status">
          {React.string(
            switch diff.target {
            | Diff(_) => "Viewing the original diff this comment was made on — read-only. "
            | Blob(_) => "Viewing the pinned original file; comments stay anchored to this source. "
            },
          )}
          {Chrome.keys(chrome, Back)->Option.mapOr(React.null, keys => <UI.Kbd keys />)}
          {React.string(" back to the current diff")}
        </div>
      : React.null}
    {diff.fileThreads->Array.length > 0
      ? <div className="file-threads">
          {React.string(Int.toString(Array.length(diff.fileThreads)) ++ " file-level thread(s)")}
        </div>
      : React.null}
    binary
    {collapsed
      ? <div className="diff-collapsed">
          {React.string(diff.viewed == Viewed ? "Viewed — " : "Collapsed — ")}
          <UI.Button
            label="show anyway"
            title=?{Chrome.tip(chrome, ToggleFileCollapse)}
            kind=Ghost
            onClick={() => dispatch(ToggleFileCollapse({file: diff.file}))}
          />
        </div>
      : React.null}
    <div
      className={"diff-scroll" ++ (collapsed ? " hidden" : "")}
      ref={ReactDOM.Ref.domRef(scrollRef)}
      onMouseLeave={_ => setDrag(_ => None)}
      onMouseUp={_ =>
        switch drag {
        | Some((start, end_)) => {
            setDrag(_ => None)
            if start != end_ {
              dispatch(CommentLines({file: diff.file, side: Head, startLine: start, endLine: end_}))
            }
          }
        | None => ()
        }}
    >
      <div
        className="diff-rows"
        style={{
          height: Int.toString(virtualizer->Virtual.getTotalSize) ++ "px",
          position: "relative",
        }}
      >
        {items
        ->Array.map(item => {
          let style: ReactDOM.Style.t = {
            position: "absolute",
            top: "0",
            left: "0",
            width: "100%",
            transform: "translateY(" ++ Int.toString(item.start) ++ "px)",
          }
          let focused = focusedRow == Some(item.index)
          let inner = switch cached->Dict.get(Int.toString(item.index)) {
          | Some(r) =>
            let scrollAnchor = focused
              ? Row.lineOn(r.row, focusedSide)->Option.map(line =>
                  Scroll.lineAnchor(~file=diff.file, ~side=focusedSide, ~line)
                )
              : None
            <>
              <Row
                row=r.row
                layout
                index=item.index
                focused
                focusedSide
                ?scrollAnchor
                drafted=?r.drafted
                threads=r.threads
                selectedSide=?{switch (visual, drag, Row.lineOn(r.row, Head)) {
                | (Some({start, end_}), _, Some(_)) if item.index >= start && item.index <= end_ =>
                  Some(Domain.Side.Head)
                | (_, Some((start, end_)), Some(line))
                  if line >= Math.Int.min(start, end_) && line <= Math.Int.max(start, end_) =>
                  Some(Domain.Side.Head)
                | _ => None
                }}
                onComment=?{Row.lineOn(r.row, Head)->Option.map(line =>
                  _ =>
                    dispatch(
                      CommentLines({file: diff.file, side: Head, startLine: line, endLine: line}),
                    )
                )}
                onClick={_ =>
                  dispatch(SetFocus({focus: Focus.Diff({row: item.index, side: Head})}))}
                onMouseDown={_ =>
                  switch Row.lineOn(r.row, Head) {
                  | Some(line) => setDrag(_ => Some((line, line)))
                  | None => ()
                  }}
                onMouseEnter={_ =>
                  switch (drag, Row.lineOn(r.row, Head)) {
                  | (Some((start, _)), Some(line)) => setDrag(_ => Some((start, line)))
                  | _ => ()
                  }}
                chrome
                onExpand={(gap, dir) => dispatch(ExpandGap({file: diff.file, gap, dir}))}
              />
              {switch (r.drafted, draft) {
              | (Some((Anchor, _)), Some({purpose: Comment(_)} as d)) =>
                <Composer chrome bindings draft=d pendingRefresh dispatch />
              | (Some(_), _) | (None, _) => React.null
              }}
              {r.threads
              ->Array.filter((t: View.RowThread.t) => t.place == Anchor)
              ->Array.map((t: View.RowThread.t) => t.thread)
              ->Array.filterMap(threadOf)
              ->Array.map(ti => {
                let thread = threads->Array.getUnsafe(ti)
                let composer = switch (replyTo, draft) {
                | (Some(id), Some(d)) if id == thread.id =>
                  <Composer chrome bindings draft=d pendingRefresh dispatch />
                | _ => React.null
                }
                <InlineThread
                  repositories
                  chrome
                  key=thread.id
                  thread
                  focused={focusedThread == Some(ti)}
                  index=ti
                  composer
                  dispatch
                />
              })
              ->React.array}
            </>
          | None =>
            Attrs.focused(
              <div className="row row-placeholder" role="row" style={{height: "20px"}}>
                {React.string("…")}
              </div>,
              focused,
            )
          }
          let el =
            <div
              key=item.key
              style
              ref={ReactDOM.Ref.callbackDomRef(el => {
                (virtualizer->Virtual.measureElement)(el)
                None
              })}
            >
              inner
            </div>
          Attrs.withData(el, [("data-index", Int.toString(item.index))])
        })
        ->React.array}
      </div>
    </div>
  </section>
}
