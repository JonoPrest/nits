// Scrolling follows the focus (§6.4): the core says which row is focused
// and, for `z z`/`z t`/`z b`, where the view should sit around it. This
// module is only the arithmetic and the DOM call that carry that out —
// no decisions of its own.

/// A vertical extent in viewport coordinates.
type box = {top: float, bottom: float, height: float}

type mode =
  /// Keep the row on screen, moving as little as possible and leaving a
  /// margin of context past it (vim's `scrolloff`): what a motion does.
  | Nearest
  /// Put the row where the reader asked for it.
  | Align(View.ScrollAlign.t)

/// How far `container` must scroll for `row` to sit as `mode` says;
/// positive scrolls down. `headroom` is the height of anything painted
/// over the top of the container (the sticky file header), and `margin`
/// the context to keep past the row.
let delta = (~container: box, ~row: box, ~headroom: float, ~margin: float, ~mode: mode): float => {
  let top = container.top +. headroom
  let bottom = container.bottom
  switch mode {
  | Align(Top) => row.top -. top
  | Align(Bottom) => row.bottom -. bottom
  | Align(Center) => row.top -. (top +. (bottom -. top -. row.height) /. 2.)
  | Nearest => {
      // A viewport barely taller than the row has no room for a margin;
      // showing the row at all beats showing context around it.
      let m = Math.min(margin, Math.max(0., (bottom -. top -. row.height) /. 2.))
      if row.top < top +. m {
        row.top -. (top +. m)
      } else if row.bottom > bottom -. m {
        row.bottom -. (bottom -. m)
      } else {
        0.
      }
    }
  }
}

/// What the host should do about the view right now.
type step =
  /// Nothing to do — or nothing that can be done yet, because the focused
  /// row has not been rendered.
  | Skip
  /// Keep the focused row on screen (`Nearest`).
  | Follow
  /// Perform a `z z`/`z t`/`z b` that has not been performed yet.
  | Reposition(View.ScrollAlign.t)
  /// A focused list item, which scrolls itself into view.
  | List

/// What to do, and the intent watermark to carry forward. `seen` is the
/// last reposition already performed and `present` says whether the
/// focused row is in the DOM: an intent whose row has not arrived yet is
/// KEPT, not consumed, so it still happens when the chunk lands. A
/// remounting view starts `seen` at the current intent, so an old one is
/// not replayed as if it were new.
let plan = (
  ~focus: View.Focus.t,
  ~scroll: option<View.ScrollIntent.t>,
  ~present: bool,
  ~seen: option<int>,
): (step, option<int>) =>
  switch focus {
  | Composer(_) | Help(_) => (Skip, seen)
  | ReviewRequest(_) | ReviewList(_) | Tree(_) | Thread(_) | CommitStepper(_) => (List, seen)
  | Diff(_) =>
    if !present {
      (Skip, seen)
    } else {
      switch scroll {
      | Some({seq, align}) if seen != Some(seq) => (Reposition(align), Some(seq))
      | Some(_) | None => (Follow, seen)
      }
    }
  }

/// Rows of context kept past the cursor (vim's default is 0; a few rows
/// is the setting people actually use).
let scrolloff = 3.

type element = Dom.element

@get external scrollTop: element => float = "scrollTop"
@set external setScrollTop: (element, float) => unit = "scrollTop"
@get external clientHeight: element => float = "clientHeight"
@get external scrollHeight: element => float = "scrollHeight"
@get external parentElement: element => Nullable.t<element> = "parentElement"
@send
external getBoundingClientRect: element => {"top": float, "bottom": float, "height": float} =
  "getBoundingClientRect"
@val @scope("document") external querySelector: string => Nullable.t<element> = "querySelector"
@send external closest: (element, string) => Nullable.t<element> = "closest"
@send external querySelectorIn: (element, string) => Nullable.t<element> = "querySelector"
@send external getAttribute: (element, string) => Nullable.t<string> = "getAttribute"

let boxOf = (el: element): box => {
  let r = getBoundingClientRect(el)
  {top: r["top"], bottom: r["bottom"], height: r["height"]}
}

/// Stable identity for a rendered line. It intentionally includes the
/// side: a split row may carry two different source lines, and preserving
/// only the row would move a base-side reader onto the head-side line.
let lineAnchor = (~file: View.FileRef.t, ~side: Domain.Side.t, ~line: int): string =>
  file.repoId ++ ":" ++ file.path ++ ":" ++ Domain.Side.name(side) ++ ":" ++ Int.toString(line)

/// The nearest ancestor that actually scrolls.
let rec scroller = (el: element): option<element> =>
  switch parentElement(el)->Nullable.toOption {
  | None => None
  | Some(p) =>
    if scrollHeight(p) > clientHeight(p) +. 1. {
      Some(p)
    } else {
      scroller(p)
    }
  }

/// The focused logical line's screen position immediately before a view
/// patch lands. The DOM element itself is not retained: React and the
/// virtualizer are free to replace it while applying the patch.
type anchor = {key: string, top: float}

let captureAnchor = (): option<anchor> =>
  switch querySelector("[data-focused][data-scroll-anchor]")->Nullable.toOption {
  | None => None
  | Some(el) =>
    getAttribute(el, "data-scroll-anchor")
    ->Nullable.toOption
    ->Option.map(key => {key, top: boxOf(el).top})
  }

/// Put the same logical line back at its captured viewport-relative Y.
/// Measuring the actual rectangles makes this independent of row index,
/// insertion direction, and the heights of rows revealed above it.
let restoreAnchor = (anchor: anchor): bool =>
  switch querySelector("[data-focused][data-scroll-anchor]")->Nullable.toOption {
  | None => false
  | Some(el)
    if getAttribute(el, "data-scroll-anchor")->Nullable.toOption != Some(anchor.key) => false
  | Some(el) =>
    switch scroller(el) {
    | None => false
    | Some(container) => {
        let d = boxOf(el).top -. anchor.top
        if d != 0. {
          setScrollTop(container, scrollTop(container) +. d)
        }
        true
      }
    }
  }

/// A file section's header is sticky, so it covers the top of the
/// scroller: a row put flush with the edge would hide behind it.
let headroomOf = (el: element): float =>
  switch closest(el, ".file-diff")->Nullable.toOption {
  | None => 0.
  | Some(section) =>
    switch querySelectorIn(section, ".file-diff-header")->Nullable.toOption {
    | Some(header) => boxOf(header).height
    | None => 0.
    }
  }

/// Scroll the focused element as `mode` says. A no-op when nothing is
/// focused, when it is not inside a scroller, or when it is already where
/// it belongs.
let apply = (mode: mode) =>
  switch querySelector("[data-focused]")->Nullable.toOption {
  | None => ()
  | Some(el) =>
    switch scroller(el) {
    | None => ()
    | Some(container) => {
        let row = boxOf(el)
        let d = delta(
          ~container=boxOf(container),
          ~row,
          ~headroom=headroomOf(el),
          ~margin=scrolloff *. row.height,
          ~mode,
        )
        if d != 0. {
          setScrollTop(container, scrollTop(container) +. d)
        }
      }
    }
  }
