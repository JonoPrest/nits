// One render-model row (§6.5, §6.6): the same `Row` in unified or split
// layout, styled only through the semantic classes in app.css.

open Render

let spanClassName = (c: SpanClass.t): string =>
  switch c {
  | Keyword => "span-keyword"
  | String => "span-string"
  | Number => "span-number"
  | Comment => "span-comment"
  | Type => "span-type"
  | Function => "span-function"
  | Variable => "span-variable"
  | Constant => "span-constant"
  | Operator => "span-operator"
  | Punctuation => "span-punctuation"
  | Attribute => "span-attribute"
  | Tag => "span-tag"
  | Other => "span-other"
  }

let rowClassName = (row: Row.t): string =>
  switch row {
  | HunkHeader(_) => "row-hunk-header"
  | Context(_) => "row-context"
  | Removed(_) => "row-removed"
  | Added(_) => "row-added"
  | Modified(_) => "row-modified"
  | Expander(_) => "row-expander"
  | WhitespaceOnly(_) => "row-whitespace-only"
  }

/// The source line represented by one side of a row. Keeping this beside
/// the renderer gives every host the same stable identity for focus and
/// scroll anchoring; row indices are deliberately not identities because
/// context expansion renumbers them.
let lineOn = (row: Row.t, side: Domain.Side.t): option<int> =>
  switch (row, side) {
  | (Context({left}) | Modified({left}) | Removed({left}), Base) => Some(left.lineNo)
  | (Context({right}) | Modified({right}) | Added({right}), Head) => Some(right.lineNo)
  | (Added(_), Base)
  | (Removed(_), Head)
  | (HunkHeader(_), _)
  | (Expander(_), _)
  | (WhitespaceOnly(_), _) =>
    None
  }

/// A cell's text split at every span and changed-range boundary, each
/// piece carrying its span class and whether it is inside a changed range.
type piece = {text: string, class: option<SpanClass.t>, changed: bool}

let pieces = (cell: Cell.t): array<piece> => {
  let len = String.length(cell.text)
  let cuts = [0, len]
  cell.spans->Array.forEach(s => {
    cuts->Array.push(s.range.start)
    cuts->Array.push(s.range.end_)
  })
  cell.changed->Array.forEach(r => {
    cuts->Array.push(r.start)
    cuts->Array.push(r.end_)
  })
  let sorted =
    cuts
    ->Array.filter(c => c >= 0 && c <= len)
    ->Array.toSorted((a, b) => Int.compare(a, b))
  let out = []
  let prev = ref(-1)
  sorted->Array.forEach(c => {
    if c != prev.contents {
      if prev.contents >= 0 && c > prev.contents {
        let start = prev.contents
        let text = String.slice(cell.text, ~start, ~end=c)
        let class =
          cell.spans
          ->Array.find(s => s.range.start <= start && c <= s.range.end_)
          ->Option.map(s => s.class)
        let changed = cell.changed->Array.some(r => r.start <= start && c <= r.end_)
        out->Array.push({text, class, changed})
      }
      prev := c
    }
  })
  out
}

/// The other half of a row.
let other = (side: Domain.Side.t): Domain.Side.t =>
  switch side {
  | Base => Head
  | Head => Base
  }

/// The half of a row a cell is: base is the removed (left) column, head
/// the added (right) one. A modified row renders both, and each is its
/// own comment target.
let sideClass = (side: Domain.Side.t): string =>
  switch side {
  | Base => "left"
  | Head => "right"
  }

let endingName = (ending: LineEnding.t) =>
  switch ending {
  | Lf => "LF"
  | CrLf => "CRLF"
  | Missing => "No final newline"
  }

// Bare CR is source content, not a line terminator. Its visible glyph comes
// from CSS so selecting source retains the original character, not the glyph.
let sourceText = text =>
  text
  ->String.split("\r")
  ->Array.mapWithIndex((text, index) =>
    <React.Fragment key={Int.toString(index)}>
      {index > 0
        ? <span className="cell-carriage-return" title="Carriage return (source content)">
            {React.string("\r")}
          </span>
        : React.null}
      {React.string(text)}
    </React.Fragment>
  )
  ->React.array

module CellView = {
  @react.component
  let make = (
    ~cell: Cell.t,
    ~side: Domain.Side.t,
    ~endingLabel: option<string>=?,
    ~focused: bool=false,
    ~selected: bool=false,
    ~commented: bool=false,
    ~drafting: bool=false,
    ~threads: int=0,
    ~onClick: unit => unit=() => (),
    ~onComment: option<unit => unit>=?,
    ~commentTip: option<string>=?,
    ~onMouseDown: unit => unit=() => (),
    ~onMouseEnter: unit => unit=() => (),
  ) => {
    // Four states can be on one cell at once (focused, selected,
    // commented, being drafted about), so each is its own class over the
    // diff-side background rather than a background of its own.
    let className =
      "cell-" ++
      sideClass(side) ++
      (focused ? " cell-focused" : "") ++
      (selected ? " cell-selected" : "") ++
      (commented ? " cell-commented" : "") ++ (drafting ? " cell-drafting" : "")
    Attrs.withData(
      <div
        className
        onClick={_ => onClick()}
        onMouseDown={_ => onMouseDown()}
        onMouseEnter={_ => onMouseEnter()}
      >
        <span className="cell-line-no"> {React.string(Int.toString(cell.lineNo))} </span>
        {switch onComment {
        | Some(comment) =>
          <button
            type_="button"
            className="btn btn-ghost cell-comment"
            ariaLabel={"Comment on line " ++ Int.toString(cell.lineNo)}
            title=?commentTip
            onMouseDown={ev => ReactEvent.Mouse.stopPropagation(ev)}
            onClick={ev => {
              ReactEvent.Mouse.stopPropagation(ev)
              comment()
            }}
          >
            {React.string("+")}
          </button>
        | None => React.null
        }}
        {pieces(cell)
        ->Array.mapWithIndex((p, i) => {
          let cls = switch (p.class, p.changed) {
          | (Some(c), true) => spanClassName(c) ++ " cell-changed"
          | (Some(c), false) => spanClassName(c)
          | (None, true) => "cell-changed"
          | (None, false) => ""
          }
          <span key={Int.toString(i)} className=cls> {sourceText(p.text)} </span>
        })
        ->React.array}
        {threads > 0
          ? <span className="cell-threads" title={Int.toString(threads) ++ " thread(s)"}>
              {React.string("💬")}
            </span>
          : React.null}
        {switch endingLabel {
        | Some(label) => <span className="cell-ending"> {React.string(label)} </span>
        | None => React.null
        }}
      </div>,
      [("data-side", Domain.Side.name(side))],
    )
  }
}

let empty = (side: Domain.Side.t) => <div className={"cell-" ++ sideClass(side) ++ " cell-empty"} />

@react.component
let make = (
  ~row: Row.t,
  ~layout: View.Layout.t,
  ~index: int,
  ~focused: bool,
  ~threads: array<View.RowThread.t>,
  ~focusedSide: Domain.Side.t=Head,
  ~selectedSide: option<Domain.Side.t>=?,
  ~drafted: option<(View.RowPlace.t, Domain.Side.t)>=?,
  ~onClick: Domain.Side.t => unit=_ => (),
  ~onComment: option<Domain.Side.t => unit>=?,
  ~onMouseDown: Domain.Side.t => unit=_ => (),
  ~onMouseEnter: Domain.Side.t => unit=_ => (),
  ~onExpand: (int, Render.ExpandDir.t) => unit=(_, _) => (),
  ~chrome: array<View.Hint.t>=[],
  ~scrollAnchor: option<string>=?,
) => {
  let base = "row " ++ rowClassName(row)
  let className = switch layout {
  | Unified => base ++ " row-unified"
  | Split => base ++ " row-split"
  }
  // The 💬 marker hangs where the card is; every line of a range is
  // marked as commented so the stretch reads as one thing.
  let anchorsOn = (side: Domain.Side.t) =>
    threads
    ->Array.filter((t: View.RowThread.t) => t.side == side && t.place == Anchor)
    ->Array.length
  let commentedOn = (side: Domain.Side.t) =>
    threads->Array.some((t: View.RowThread.t) => t.side == side)
  let draftingOn = (side: Domain.Side.t) =>
    switch drafted {
    | Some((_, s)) => s == side
    | None => false
    }
  // One rendered cell standing for both sides: a unified context row has
  // a line on each side but shows only one. Added and removed rows are
  // NOT this — they genuinely have no cell on the other side, so they are
  // not focused or selected when the other side is the target.
  let oneCellBothSides = switch (row, layout) {
  | (Context(_), Unified) => true
  | (Context(_), Split)
  | (Modified(_), Unified | Split)
  | (Added(_), Unified | Split)
  | (Removed(_), Unified | Split)
  | (HunkHeader(_), _)
  | (Expander(_), _)
  | (WhitespaceOnly(_), _) => false
  }
  let onSide = (side: Domain.Side.t, target: Domain.Side.t) => side == target || oneCellBothSides
  let endingLabel = (cell: Cell.t) =>
    switch (row, layout) {
    | (Context({left, right}), Unified) if left.ending != right.ending =>
      Some("Line ending: " ++ endingName(left.ending) ++ " → " ++ endingName(right.ending))
    | (Modified({left, right}), _) if left.ending != right.ending => Some(endingName(cell.ending))
    | _ =>
      switch cell.ending {
      | Lf => None
      | CrLf | Missing => Some(endingName(cell.ending))
      }
    }
  let cell = (~cell: Cell.t, ~side: Domain.Side.t) =>
    <CellView
      cell
      side
      endingLabel=?{endingLabel(cell)}
      focused={focused && onSide(side, focusedSide)}
      selected={switch selectedSide {
      | Some(target) => onSide(side, target)
      | None => false
      }}
      commented={commentedOn(side) || (oneCellBothSides && commentedOn(other(side)))}
      drafting={draftingOn(side) || (oneCellBothSides && draftingOn(other(side)))}
      threads={anchorsOn(side) + (oneCellBothSides ? anchorsOn(other(side)) : 0)}
      onComment=?{onComment->Option.map(comment => () => comment(side))}
      commentTip=?{Chrome.tip(chrome, Comment)}
      onClick={() => onClick(side)}
      onMouseDown={() => onMouseDown(side)}
      onMouseEnter={() => onMouseEnter(side)}
    />
  let body = switch (row, layout) {
  | (HunkHeader({text}), _) => <div className="cell-hunk"> {React.string(text)} </div>
  | (WhitespaceOnly(_), _) =>
    <div className="cell-hunk"> {React.string("whitespace-only changes hidden")} </div>
  | (Expander({hidden, dir, gap}), _) => {
      // One control per direction the gap can open in, so the mouse can
      // say what `z u`/`z d` say. A `Both` gap opens from either end.
      let arrow = (d: Render.ExpandDir.t) =>
        switch d {
        | Up => "↑"
        | Down => "↓"
        | Both => "↕"
        }
      // Tooltips come from the keymap: these arrows are the mouse alias
      // of `z u`/`z d` and must say so even after a rebinding.
      let button = (d: Render.ExpandDir.t) =>
        <button
          type_="button"
          className="btn btn-ghost expander-arrow"
          title=?{Chrome.tip(
            chrome,
            switch d {
            | Up => ExpandUp
            | Down | Both => ExpandDown
            },
          )}
          onClick={ev => {
            ReactEvent.Mouse.stopPropagation(ev)
            onExpand(gap, d)
          }}
        >
          {React.string(arrow(d))}
        </button>
      <div
        className="cell-hunk"
        onClick={ev => {
          // The expander is an action beside the cursor, not a row to
          // move the cursor onto. This keeps its mouse alias identical to
          // `z u`/`z d` while the async render is in flight.
          ReactEvent.Mouse.stopPropagation(ev)
          onExpand(gap, dir)
        }}
      >
        {switch dir {
        | Both =>
          <>
            {button(Up)}
            {button(Down)}
          </>
        | Up => button(Up)
        | Down => button(Down)
        }}
        {React.string(" " ++ Int.toString(hidden) ++ " more lines — expand")}
      </div>
    }
  | (Context({right}), Unified) => cell(~cell=right, ~side=Head)
  | (Context({left, right}), Split) =>
    <>
      {cell(~cell=left, ~side=Base)}
      {cell(~cell=right, ~side=Head)}
    </>
  | (Removed({left}), Unified) => cell(~cell=left, ~side=Base)
  | (Removed({left}), Split) =>
    <>
      {cell(~cell=left, ~side=Base)}
      {empty(Head)}
    </>
  | (Added({right}), Unified) => cell(~cell=right, ~side=Head)
  | (Added({right}), Split) =>
    <>
      {empty(Base)}
      {cell(~cell=right, ~side=Head)}
    </>
  | (Modified({left, right}), Unified | Split) =>
    <>
      {cell(~cell=left, ~side=Base)}
      {cell(~cell=right, ~side=Head)}
    </>
  }
  // A row with no cells (a hunk header) still takes a click, on the side
  // the focus is already on.
  let rowClick = switch row {
  | HunkHeader(_) | Expander(_) | WhitespaceOnly(_) => _ => onClick(focusedSide)
  | Context(_) | Added(_) | Removed(_) | Modified(_) => _ => ()
  }
  Attrs.withData(
    <div className role="row" onClick=rowClick> body </div>,
    focused
      ? [
          ("data-focused", "true"),
          ("data-row-index", Int.toString(index)),
          ("data-side", Domain.Side.name(focusedSide)),
        ]->Array.concat(
          scrollAnchor->Option.map(key => [("data-scroll-anchor", key)])->Option.getOr([]),
        )
      : [("data-row-index", Int.toString(index))],
  )
}
