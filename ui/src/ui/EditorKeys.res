// Text editors intercept only their configured semantic commands. The same
// resolver serves creation and comment editors; ordinary text and IME remain native.
let composing: ReactEvent.Keyboard.t => bool = %raw(`event =>
  !!(event.nativeEvent?.isComposing || event.nativeEvent?.keyCode === 229)`)

let consume = (~pending, ~bindings, ~allowed, ~run, ev: ReactEvent.Keyboard.t): bool => {
  if composing(ev) {
    pending.KeySequence.keys = []
    false
  } else {
    switch Keys.ofBrowser({
      key: ReactEvent.Keyboard.key(ev),
      ctrlKey: ReactEvent.Keyboard.ctrlKey(ev),
      altKey: ReactEvent.Keyboard.altKey(ev),
      shiftKey: ReactEvent.Keyboard.shiftKey(ev),
      metaKey: ReactEvent.Keyboard.metaKey(ev),
    }) {
    | None => false
    | Some(chord) =>
      switch KeySequence.step(
        pending,
        bindings->Array.filter(h => allowed(h.View.Hint.command)),
        chord,
      ) {
      | Runs(command) =>
        ReactEvent.Keyboard.preventDefault(ev)
        run(command)
        true
      | Prefix =>
        ReactEvent.Keyboard.preventDefault(ev)
        true
      | Unbound => false
      }
    }
  }
}

let handle = (~pending, ~bindings, ~allowed, ~run, ev) => {
  let _ = consume(~pending, ~bindings, ~allowed, ~run, ev)
  ReactEvent.Keyboard.stopPropagation(ev)
}
