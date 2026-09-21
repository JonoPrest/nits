// Key chords (client-core `keymap.rs`) and the browser → chord normaliser
// (plan 4.5). The core resolves chords; the UI only captures them.

module NamedKey = {
  @schema
  type t =
    | Enter
    | Esc
    | Tab
    | Backspace
    | Space
    | Up
    | Down
    | Left
    | Right
    | PageUp
    | PageDown
    | Home
    | End
}

module NamedKeyName = {
  /// The keymap's spelling of a named key, as bindings are written.
  let of_ = (key: NamedKey.t): string =>
    switch key {
    | Enter => "enter"
    | Esc => "esc"
    | Tab => "tab"
    | Backspace => "backspace"
    | Space => "space"
    | Up => "up"
    | Down => "down"
    | Left => "left"
    | Right => "right"
    | PageUp => "pageup"
    | PageDown => "pagedown"
    | Home => "home"
    | End => "end"
    }
}

module KeyCode = {
  @schema @tag("type")
  type t =
    | @as("Char") Char({c: string})
    | @as("Named") Named({key: NamedKey.t})
}

module Modifiers = {
  @schema
  type t = {ctrl: bool, alt: bool, shift: bool, meta: bool}
}

module KeyChord = {
  @schema
  type t = {key: KeyCode.t, mods: Modifiers.t}
}

/// What a browser `KeyboardEvent` gives us.
type browserKey = {
  key: string,
  ctrlKey: bool,
  altKey: bool,
  shiftKey: bool,
  metaKey: bool,
}

/// `KeyboardEvent.key` → chord. Printable keys become `Char` (shift is
/// implied by the character, so it is dropped); named keys carry their
/// modifiers. Modifier-only presses and unknown keys are `None`.
let ofBrowser = (ev: browserKey): option<KeyChord.t> => {
  let named: option<NamedKey.t> = switch ev.key {
  | "Enter" => Some(Enter)
  | "Escape" => Some(Esc)
  | "Tab" => Some(Tab)
  | "Backspace" => Some(Backspace)
  | " " => Some(Space)
  | "ArrowUp" => Some(Up)
  | "ArrowDown" => Some(Down)
  | "ArrowLeft" => Some(Left)
  | "ArrowRight" => Some(Right)
  | "PageUp" => Some(PageUp)
  | "PageDown" => Some(PageDown)
  | "Home" => Some(Home)
  | "End" => Some(End)
  | _ => None
  }
  switch named {
  | Some(key) =>
    Some({
      key: Named({key: key}),
      mods: {ctrl: ev.ctrlKey, alt: ev.altKey, shift: ev.shiftKey, meta: ev.metaKey},
    })
  | None =>
    if String.length(ev.key) == 1 {
      Some({
        key: Char({c: ev.key}),
        mods: {ctrl: ev.ctrlKey, alt: ev.altKey, shift: false, meta: ev.metaKey},
      })
    } else {
      None
    }
  }
}

let toJson = (chord: KeyChord.t): JSON.t => S.reverseConvertToJsonOrThrow(chord, KeyChord.schema)

/// The keymap spelling used in the core's applicable bindings.
let text = (chord: KeyChord.t): string => {
  let key = switch chord.key {
  | Char({c}) => c == " " ? "space" : c
  | Named({key}) => NamedKeyName.of_(key)
  }
  (chord.mods.ctrl ? "ctrl+" : "") ++
  (chord.mods.alt ? "alt+" : "") ++
  (chord.mods.shift ? "shift+" : "") ++
  (chord.mods.meta ? "meta+" : "") ++
  key
}
