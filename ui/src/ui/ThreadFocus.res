// Core focus selects the thread; the DOM follows it to a focusable card.
// Native Tab visits its Markdown links, then the keymap owns pane
// navigation again at the boundary. Enter on the card still reaches the
// core's Open command; only Enter on a link activates that link.

type elements
@send external querySelectorAll: (Dom.element, string) => elements = "querySelectorAll"
@get external length: elements => int = "length"
@send external item: (elements, int) => Nullable.t<Dom.element> = "item"
@send external contains: (Dom.element, Dom.element) => bool = "contains"
@send external matches: (Dom.element, string) => bool = "matches"
@send external focus: Dom.element => unit = "focus"
@send external blur: Dom.element => unit = "blur"
@get external target: ReactEvent.Keyboard.t => Dom.element = "target"
@val @scope("document") external activeElement: Nullable.t<Dom.element> = "activeElement"

let use = (~focused: bool) => {
  let root = React.useRef(Nullable.null)
  React.useEffect1(() => {
    switch (focused, root.current->Nullable.toOption) {
    | (true, Some(card)) => {
        if !(activeElement->Nullable.toOption->Option.mapOr(false, el => contains(card, el))) {
          focus(card)
        }
        Some(
          () => {
            // A core motion to another thread/pane must not leave its old
            // link owning Enter. Inputs (including a new inline composer)
            // keep their own focus.
            switch activeElement->Nullable.toOption {
            | Some(el)
              if contains(card, el) && (el == card || matches(el, ".thread-body a[href]")) =>
              blur(el)
            | Some(_) | None => ()
            }
          },
        )
      }
    | (false, _) | (true, None) => None
    }
  }, [focused])

  let onKeyDown = (ev: ReactEvent.Keyboard.t) => {
    if (
      focused &&
      ReactEvent.Keyboard.key(ev) == "Tab" &&
      !ReactEvent.Keyboard.ctrlKey(ev) &&
      !ReactEvent.Keyboard.altKey(ev) &&
      !ReactEvent.Keyboard.metaKey(ev)
    ) {
      switch root.current->Nullable.toOption {
      | Some(card) => {
          let links = querySelectorAll(card, ".thread-body a[href]")
          let count = length(links)
          let origin = target(ev)
          let reverse = ReactEvent.Keyboard.shiftKey(ev)
          let last = item(links, count - 1)->Nullable.toOption
          // The card precedes its first link in the native tab order,
          // including Shift+Tab back from that link. With no links there
          // is no extra stop: Tab immediately remains a core chord.
          let inside =
            count > 0 &&
              ((origin == card && !reverse) ||
                (contains(card, origin) &&
                matches(origin, ".thread-body a[href]") &&
                (reverse || last != Some(origin))))
          if inside {
            ReactEvent.Keyboard.stopPropagation(ev)
          }
        }
      | None => ()
      }
    }
  }
  (root, onKeyDown)
}
