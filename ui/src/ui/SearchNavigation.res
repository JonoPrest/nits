// Shared, explicit focus model for every searchable surface. Result
// selection belongs to its owner (the core for file/content searches).
type zone = Input | Results | Controls

type insertion = {text: string, cursor: int}

@send external focus: Dom.element => unit = "focus"
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

let closeOnEscape = (close, ev) => {
  if ReactEvent.Keyboard.key(ev) == "Escape" {
    ReactEvent.Keyboard.preventDefault(ev)
    ReactEvent.Keyboard.stopPropagation(ev)
    close()
  }
}

let useNavigation = (~count, ~selected, ~step, ~query, ~change, ~submit, ~close) => {
  let (zone, setZone) = React.useState(() => Input)
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
    if zone == Results {
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
    let key = ReactEvent.Keyboard.key(ev)
    switch key {
    | "ArrowDown" | "Tab" if !ReactEvent.Keyboard.shiftKey(ev) && count > 0 => {
        ReactEvent.Keyboard.preventDefault(ev)
        step(-(selected->Option.getOr(0)))
        setZone(_ => Results)
      }
    | "Tab" => setZone(_ => Controls)
    | "ArrowUp" => ReactEvent.Keyboard.preventDefault(ev)
    | "Enter" => {
        ReactEvent.Keyboard.preventDefault(ev)
        submit()
      }
    | "Escape" => {
        ReactEvent.Keyboard.preventDefault(ev)
        close()
      }
    | _ => ()
    }
  }
  let onResultsKey = ev => {
    ReactEvent.Keyboard.stopPropagation(ev)
    let key = ReactEvent.Keyboard.key(ev)
    let plain =
      !ReactEvent.Keyboard.ctrlKey(ev) &&
      !ReactEvent.Keyboard.metaKey(ev) &&
      !ReactEvent.Keyboard.altKey(ev)
    let handled = switch key {
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
    | "Escape" => {
        close()
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
    if handled {
      ReactEvent.Keyboard.preventDefault(ev)
    }
  }
  let onResultsFocus = () => {
    if count > 0 {
      setZone(_ => Results)
    }
  }
  (selected, inputRef, resultsRef, toInput, onResultsFocus, onChange, onInputKey, onResultsKey)
}
