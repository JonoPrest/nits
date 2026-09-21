// One file of the stacked diff view (UI-DESIGN §Layout, GitHub-style):
// collapsible header with path · copy · +A −D · comment · Viewed ·
// expand, then every cached row with inline threads. Dragging across
// line numbers opens a multiline comment draft (`CommentLines`).

open View

/// The line `side` of `row` carries, mirroring the core's `line_on`: a
/// modified row has one on each side, an added row only on head, a
/// removed row only on base.
let lineOn = Row.lineOn

@val @scope(("navigator", "clipboard")) external writeText: string => unit = "writeText"
@send external scrollIntoView: (Dom.element, {"block": string}) => unit = "scrollIntoView"

type drag = {side: Domain.Side.t, start: int, current: int}

@react.component
let make = (
  ~diff: DiffView.t,
  ~layout: Layout.t,
  ~focus: Focus.t,
  ~threads: array<ThreadView.t>,
  ~draft: option<Draft.t>,
  ~pendingRefresh: bool,
  ~isOpen: bool,
  ~chrome: array<View.Hint.t>=[],
  ~visual: option<VisualView.t>=?,
  ~dispatch: Action.t => unit,
) => {
  // Fold state is core-owned (`z a` toggles; motions skip folded files).
  let collapsed = diff.collapsed
  let (drag, setDrag) = React.useState(() => None)
  // Patches are viewport-bounded (§6.3): accumulate every row this file
  // has ever shown so scrolling/loading only ever adds rows.
  let key = DiffSeen.fileKey(diff)
  let prevKey = React.useRef("")
  let seen = React.useRef(Dict.make())
  seen.current = DiffSeen.mergeSeen(seen.current, prevKey.current, key, diff.rows)
  prevKey.current = key
  let rows =
    seen.current
    ->Dict.valuesToArray
    ->Array.toSorted((a: DiffRow.t, b: DiffRow.t) => Int.compare(a.index, b.index))
  let threadOf = (id: Ids.threadId) => threads->Array.findIndexOpt(t => t.id == id)
  let focusedThread = switch focus {
  | Thread({index}) => Some(index)
  | _ => None
  }
  let focusedRow = switch focus {
  | Diff({row}) if isOpen => Some(row)
  | _ => None
  }
  let focusedSide = switch focus {
  | Diff({side}) => side
  | _ => Domain.Side.Head
  }
  let replyTo = draft->Option.flatMap(d => View.Draft.thread(d))
  let total = switch diff.content {
  | Text({totalRows}) => totalRows
  | Binary(_) | Submodule(_) => 0
  }
  let stats = switch diff.content {
  | Text({additions, deletions}) =>
    <span className="file-stats">
      <span className="stat-add"> {React.string("+" ++ Int.toString(additions))} </span>
      <span className="stat-del"> {React.string("−" ++ Int.toString(deletions))} </span>
    </span>
  | Binary(_) | Submodule(_) => React.null
  }
  // Visual-mode selection (core-owned): only the open file's rows.
  // Visual-mode and drag selections both live on one side, and a row is
  // selected only on that side (a modified row's other half is not).
  let inVisual = (index: int): option<Domain.Side.t> =>
    switch visual {
    | Some({start, end_, side}) if isOpen && index >= start && index <= end_ => Some(side)
    | _ => None
    }
  let inDrag = (row: Render.Row.t): option<Domain.Side.t> =>
    switch drag {
    | Some({side, start, current}) =>
      switch lineOn(row, side) {
      | Some(line)
        if line >= Math.Int.min(start, current) && line <= Math.Int.max(start, current) =>
        Some(side)
      | Some(_) | None => None
      }
    | None => None
    }
  <section className="file-diff" ariaLabel=diff.file.path>
    {Attrs.focused(
      <header className="file-diff-header">
        <button
          type_="button"
          className="btn btn-ghost file-chevron"
          title={collapsed ? "expand file section" : "collapse file section"}
          onClick={_ => dispatch(ToggleFileCollapse({file: diff.file}))}
        >
          {React.string(collapsed ? "▸" : "▾")}
        </button>
        <span className="file-path mono"> {React.string(diff.file.path)} </span>
        <UI.CopyPath path={diff.file.path} chrome dispatch />
        stats
        {diff.fileThreads->Array.length > 0
          ? <span className="tree-threads">
              {React.string(Int.toString(Array.length(diff.fileThreads)))}
            </span>
          : React.null}
        <span className="file-diff-actions">
          <button
            type_="button"
            className="btn btn-ghost"
            title="comment on this file"
            onClick={_ => dispatch(CommentFile({file: diff.file}))}
          >
            {React.string("💬")}
          </button>
          <UI.Button
            label="expand file"
            kind=Ghost
            title="show the whole file as context"
            onClick={() => dispatch(ExpandContext({file: diff.file, full: true}))}
          />
          <label className="chip-check" title="mark viewed (v)">
            <input
              type_="checkbox"
              checked={diff.viewed == Viewed}
              onChange={_ => {
                dispatch(
                  diff.viewed == Viewed
                    ? UnmarkViewed({file: diff.file})
                    : MarkViewed({file: diff.file}),
                )
              }}
            />
            {React.string("Viewed")}
          </label>
        </span>
      </header>,
      isOpen && collapsed,
    )}
    {collapsed
      ? React.null
      : <div className="file-diff-body" onMouseLeave={_ => setDrag(_ => None)}>
          {switch diff.content {
          | Submodule(_) => <SubmoduleView target=diff.target />
          | Binary(_) => <div className="diff-binary"> {React.string("binary file")} </div>
          | Text(_) => React.null
          }}
          {rows
          ->Array.map(r => {
            let focused = focusedRow == Some(r.index)
            let scrollAnchor = focused
              ? lineOn(r.row, focusedSide)->Option.map(line =>
                  Scroll.lineAnchor(~file=diff.file, ~side=focusedSide, ~line)
                )
              : None
            let selectedSide = switch inDrag(r.row) {
            | Some(side) => Some(side)
            | None => inVisual(r.index)
            }
            let rowEl =
              <Row
                row=r.row
                layout
                index=r.index
                focused
                focusedSide
                ?scrollAnchor
                ?selectedSide
                drafted=?r.drafted
                threads=r.threads
                onClick={side => {
                  dispatch(Viewport({file: diff.file, firstRow: r.index, lastRow: r.index + 59}))
                  dispatch(SetFocus({focus: Focus.Diff({row: r.index, side})}))
                }}
                onMouseDown={side =>
                  switch lineOn(r.row, side) {
                  | Some(line) => setDrag(_ => Some({side, start: line, current: line}))
                  | None => ()
                  }}
                onMouseEnter={side =>
                  switch (drag, lineOn(r.row, side)) {
                  | (Some(d), Some(line)) if side == d.side =>
                    setDrag(_ => Some({...d, current: line}))
                  | (Some(_), Some(_) | None) | (None, _) => ()
                  }}
                chrome
                onExpand={(gap, dir) => dispatch(ExpandGap({file: diff.file, gap, dir}))}
              />
            <div
              key={Int.toString(r.index)}
              onMouseUp={_ =>
                switch drag {
                | Some({side, start, current}) => {
                    setDrag(_ => None)
                    if start != current {
                      dispatch(
                        CommentLines({
                          file: diff.file,
                          side,
                          startLine: Math.Int.min(start, current),
                          endLine: Math.Int.max(start, current),
                        }),
                      )
                    }
                  }
                | None => ()
                }}
            >
              rowEl
              // A fresh comment is written where it will be read: the
              // composer takes the slot the thread will occupy, under the
              // last line of its range (UI-DESIGN §Comments).
              {switch (r.drafted, draft) {
              | (Some((Anchor, _)), Some({purpose: Comment(_)} as d)) =>
                <Composer chrome draft=d pendingRefresh dispatch />
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
                  <Composer chrome draft=d pendingRefresh dispatch />
                | _ => React.null
                }
                <InlineThread
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
            </div>
          })
          ->React.array}
          {switch (Array.length(rows), total) {
          // More rows exist than we hold: load onward from the last seen
          // row (this makes the file the open one, so its viewport streams).
          | (have, total) if have > 0 && have < total =>
            <button
              type_="button"
              className="btn load-more"
              onClick={_ => {
                let from = (rows->Array.getUnsafe(have - 1)).index + 1
                dispatch(Viewport({file: diff.file, firstRow: from, lastRow: from + 199}))
              }}
            >
              {React.string("Load more (" ++ Int.toString(total - have) ++ " rows below)")}
            </button>
          | _ => React.null
          }}
        </div>}
  </section>
}
