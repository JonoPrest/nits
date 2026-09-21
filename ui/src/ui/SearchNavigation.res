// Shared, explicit focus model for every searchable surface. Result
// selection belongs to its owner (the core for file/content searches).
type zone = Input | Results | Controls

type insertion = {text: string, cursor: int}

let focus: Dom.element => unit = %raw(`el => el.focus({preventScroll: true})`)
let insertText: (Dom.element, string, string) => insertion = %raw(`(input, query, text) => {
  const start = input.selectionStart ?? query.length;
  const end = input.selectionEnd ?? start;
  return {text: query.slice(0, start) + text + query.slice(end), cursor: start + text.length};
}`)
let scrollSelected: Dom.element => unit = %raw(`el => {
  const selected = el.querySelector('[aria-selected="true"]');
  selected?.scrollIntoView?.({block: "nearest"});
}`)
let isPrintable: string => bool = %raw(`key => Array.from(key).length === 1`)

@send external setSelectionRange: (Dom.element, int, int) => unit = "setSelectionRange"

let useNavigation = (
  ~count,
  ~selected,
  ~first,
  ~step,
  ~query,
  ~change,
  ~submit,
  ~close,
  ~bindings,
) => {
  let pending = React.useRef(KeySequence.make())
  let closeKey = ev =>
    EditorKeys.consume(
      ~pending=pending.current,
      ~bindings,
      ~allowed=command => command == Back,
      ~run=_ => close(),
      ev,
    )
  let onDialogKey = ev => {
    if closeKey(ev) || ReactEvent.Keyboard.key(ev) == "Tab" {
      ReactEvent.Keyboard.stopPropagation(ev)
    }
  }
  let (zone, setZone) = React.useState(() => Input)
  // Pointer focus must not reveal the old keyboard selection underneath the
  // press. A blur or keyboard action restores keyboard scrolling.
  let pointerEntry = React.useRef(false)
  let inputRef = React.useRef(Nullable.null)
  let resultsRef = React.useRef(Nullable.null)
  let insertion = React.useRef(None)
  let zone = count == 0 && zone == Results ? Input : zone
  let selected = count == 0 ? None : Some(Math.Int.min(Math.Int.max(selected, 0), count - 1))
  React.useEffect1(() => {
    if count == 0 {
      setZone(zone => zone == Results ? Input : zone)
    }
    None
  }, [count])
  React.useEffect1(() => {
    let target = switch zone {
    | Input => inputRef.current
    | Results => resultsRef.current
    | Controls => Nullable.null
    }
    target->Nullable.toOption->Option.forEach(focus)
    None
  }, [zone])
  React.useEffect2(() => {
    if zone == Results && !pointerEntry.current {
      resultsRef.current->Nullable.toOption->Option.forEach(scrollSelected)
    }
    None
  }, (zone, selected))
  React.useEffect2(() => {
    switch (insertion.current, inputRef.current->Nullable.toOption) {
    | (Some({text, cursor}), Some(input)) if zone == Input && text == query => {
        input->setSelectionRange(cursor, cursor)
        insertion.current = None
      }
    | _ => ()
    }
    None
  }, (zone, query))
  let toInput = () => setZone(_ => Input)
  let onChange = text => {
    toInput()
    change(text)
  }
  let onInputKey = ev => {
    pointerEntry.current = false
    let key = ReactEvent.Keyboard.key(ev)
    if !closeKey(ev) && !EditorKeys.composing(ev) {
      switch key {
      | "ArrowDown" | "Tab" if !ReactEvent.Keyboard.shiftKey(ev) && count > 0 => {
          ReactEvent.Keyboard.preventDefault(ev)
          first()
          setZone(_ => Results)
        }
      | "Tab" => setZone(_ => Controls)
      | "ArrowUp" => ReactEvent.Keyboard.preventDefault(ev)
      | "Enter" => {
          ReactEvent.Keyboard.preventDefault(ev)
          submit()
        }
      | _ => ()
      }
    }
  }
  let onResultsKey = ev => {
    pointerEntry.current = false
    ReactEvent.Keyboard.stopPropagation(ev)
    let key = ReactEvent.Keyboard.key(ev)
    let plain =
      !ReactEvent.Keyboard.ctrlKey(ev) &&
      !ReactEvent.Keyboard.metaKey(ev) &&
      !ReactEvent.Keyboard.altKey(ev)
    let handled = if closeKey(ev) {
      true
    } else if EditorKeys.composing(ev) {
      false
    } else {
      switch key {
      | "Tab" if ReactEvent.Keyboard.shiftKey(ev) => {
          toInput()
          true
        }
      | "Tab" => {
          setZone(_ => Controls)
          false
        }
      | "ArrowUp" | "k" if plain => {
          if selected == Some(0) {
            toInput()
          } else {
            step(-1)
          }
          true
        }
      | "ArrowDown" | "j" if plain => {
          step(1)
          true
        }
      | "Enter" => {
          submit()
          true
        }
      | key if plain && isPrintable(key) => {
          let next =
            inputRef.current
            ->Nullable.toOption
            ->Option.mapOr({text: query ++ key, cursor: String.length(query ++ key)}, input =>
              insertText(input, query, key)
            )
          insertion.current = Some(next)
          onChange(next.text)
          true
        }
      | _ => false
      }
    }
    if handled {
      ReactEvent.Keyboard.preventDefault(ev)
    }
  }
  let onResultsFocus = () => {
    if count > 0 {
      setZone(_ => Results)
    }
  }
  let onResultsPointer = () => pointerEntry.current = true
  let onResultsBlur = () => pointerEntry.current = false
  (
    selected,
    inputRef,
    resultsRef,
    toInput,
    onResultsFocus,
    onResultsPointer,
    onResultsBlur,
    onChange,
    onInputKey,
    onResultsKey,
    onDialogKey,
  )
}
