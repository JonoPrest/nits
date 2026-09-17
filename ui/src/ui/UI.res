// The design system (after the Envio UI's `UI.res`): every shared primitive
// lives here and takes configuration props (`~kind`, `~tone`, `~gap`), never
// a `className` escape hatch. Tailwind's scanner needs class literals, so
// variants are spelled out rather than interpolated.

module Box = {
  type direction = Row | Column
  type gap = NoGap | Xs | Sm | Md
  @react.component
  let make = (~children, ~direction=Column, ~gap=NoGap, ~grow=false) => {
    let dir = switch direction {
    | Row => "flex flex-row items-center"
    | Column => "flex flex-col"
    }
    let gapClass = switch gap {
    | NoGap => ""
    | Xs => " gap-1"
    | Sm => " gap-2"
    | Md => " gap-4"
    }
    let growClass = grow ? " min-h-0 flex-1" : ""
    <div className={dir ++ gapClass ++ growClass}> children </div>
  }
}

module Panel = {
  /// A bordered region with a header; `grow` fills the remaining height.
  @react.component
  let make = (
    ~title: string,
    ~children,
    ~grow=false,
    ~actions=React.null,
    ~role=?,
    ~ariaLabel=?,
  ) => {
    let className = "panel flex flex-col" ++ (grow ? " min-h-0 flex-1" : "")
    <section className ?role ?ariaLabel>
      <header className="panel-header">
        {React.string(title)}
        actions
      </header>
      children
    </section>
  }
}

module Button = {
  type kind = Primary | Secondary | Ghost | Icon
  /// Chord aliases leave the thread's native Markdown link tab order intact.
  type navigation = Native | Chord
  @react.component
  let make = (
    ~label: string,
    ~onClick: unit => unit,
    ~kind=Secondary,
    ~navigation=Native,
    ~title=?,
    ~ariaLabel=?,
    ~ariaControls=?,
    ~expanded: option<bool>=?,
    ~hasPopup=?,
  ) => {
    let className = switch kind {
    | Primary => "btn btn-primary"
    | Secondary => "btn"
    | Ghost => "btn btn-ghost"
    | Icon => "btn btn-icon"
    }
    // Explicit type: inside a <form> a bare <button> would submit it.
    // Activation is owned here because the shell's window keymap also
    // listens for Enter and Space. Stopping both keys prevents one gesture
    // from running a focused core command as well as clicking the button.
    <button
      type_="button"
      tabIndex=?{navigation == Chord ? Some(-1) : None}
      className
      ?title
      ?ariaLabel
      ?ariaControls
      ariaExpanded=?expanded
      ariaHaspopup=?hasPopup
      onClick={_ => onClick()}
      onKeyDown={ev => {
        let key = ReactEvent.Keyboard.key(ev)
        if key == "Enter" || key == " " {
          ReactEvent.Keyboard.preventDefault(ev)
          ReactEvent.Keyboard.stopPropagation(ev)
          onClick()
        }
      }}
    >
      {kind == Icon ? <span ariaHidden=true> {React.string(label)} </span> : React.string(label)}
    </button>
  }
}

/// Copy a file's path, wherever a header shows one. The click is the
/// mouse alias of `y`: it dispatches the same action, so the core decides
/// what is copied and says so once (`ViewModel.notice`).
module CopyPath = {
  @react.component
  let make = (~path: string, ~chrome: array<View.Hint.t>=[], ~dispatch: Action.t => unit) =>
    <Button
      label="⧉"
      kind=Ghost
      title=?{Chrome.tip(chrome, CopyPath)}
      onClick={() => dispatch(CopyPath({path: path}))}
    />
}

/// Shared thread/reply affordance, with the same keymap-derived binding.
module CopyReference = {
  @react.component
  let make = (
    ~reference: option<string>,
    ~chrome: array<View.Hint.t>,
    ~dispatch: Action.t => unit,
  ) =>
    switch reference {
    | Some(reference) =>
      <Button
        label="Copy reference"
        kind=Ghost
        navigation=Chord
        title=?{Chrome.tip(chrome, CopyReference)}
        onClick={() => dispatch(CopyReference({reference: reference}))}
      />
    | None => React.null
    }
}

module Kbd = {
  /// `space` renders as the ␣ glyph everywhere a key is shown.
  @react.component
  let make = (~keys: string) => {
    let text =
      keys
      ->String.split(" ")
      ->Array.map(tok => tok == "space" ? "␣" : tok)
      ->Array.join(" ")
    <kbd> {React.string(text)} </kbd>
  }
}

module MenuItem = {
  /// A checked menu choice with roving focus. The owning menu interprets
  /// navigation keys so one primitive works for radio groups of any shape.
  @react.component
  let make = (
    ~label: string,
    ~checked: bool,
    ~tabIndex: int,
    ~onClick: unit => unit,
    ~onFocus: unit => unit,
    ~onKey: string => unit,
    ~hint: option<string>=?,
    ~title=?,
    ~autoFocus=false,
  ) =>
    <button
      type_="button"
      className="menu-item"
      role="menuitemradio"
      ariaLabel=label
      ariaChecked={checked ? #"true" : #"false"}
      tabIndex
      ?title
      autoFocus
      onClick={_ => onClick()}
      onFocus={_ => onFocus()}
      onKeyDown={ev => {
        let key = ReactEvent.Keyboard.key(ev)
        if key == "Tab" {
          // Close the owning menu without cancelling native forward or
          // reverse focus traversal.
          ReactEvent.Keyboard.stopPropagation(ev)
          onKey(key)
        } else if (
          [
            "ArrowDown",
            "ArrowRight",
            "ArrowUp",
            "ArrowLeft",
            "Enter",
            " ",
            "Escape",
          ]->Array.includes(key)
        ) {
          ReactEvent.Keyboard.preventDefault(ev)
          ReactEvent.Keyboard.stopPropagation(ev)
          onKey(key)
        }
      }}
    >
      <span className="menu-item-check" ariaHidden=true>
        {React.string(checked ? "✓" : "")}
      </span>
      <span className="menu-item-label"> {React.string(label)} </span>
      {switch hint {
      | Some(keys) =>
        <span className="menu-item-hint" ariaHidden=true>
          <Kbd keys />
        </span>
      | None => React.null
      }}
    </button>
}

module Empty = {
  @react.component
  let make = (~text: string) => <p className="empty"> {React.string(text)} </p>
}

module Badge = {
  type tone = Neutral | Add | Remove | Accent
  @react.component
  let make = (~text: string, ~tone=Neutral) => {
    let className = switch tone {
    | Neutral => "badge"
    | Add => "badge badge-add"
    | Remove => "badge badge-remove"
    | Accent => "badge badge-accent"
    }
    <span className> {React.string(text)} </span>
  }
}

module Select = {
  /// A native select over `(value, label)` options.
  @react.component
  let make = (
    ~value: string,
    ~options: array<(string, string)>,
    ~onChange: string => unit,
    ~ariaLabel=?,
  ) =>
    <select
      className="text-input"
      value
      ?ariaLabel
      onChange={ev => onChange(ReactEvent.Form.target(ev)["value"])}
    >
      {options
      ->Array.map(((v, label)) => <option key=v value=v> {React.string(label)} </option>)
      ->React.array}
    </select>
}

module TextInput = {
  /// A text field whose keys never reach the keymap; `onKey` sees named
  /// keys (Enter, Escape) first.
  @react.component
  let make = (
    ~value: string,
    ~onChange: string => unit,
    ~placeholder: string,
    ~autoFocus=false,
    ~onKey: string => unit=_ => (),
    ~preventKeys: array<string>=[],
    ~inputRef=?,
    ~onFocus: unit => unit=() => (),
    ~onKeyEvent: ReactEvent.Keyboard.t => unit=_ => (),
  ) =>
    <input
      className="text-input"
      ref=?inputRef
      onFocus={_ => onFocus()}
      autoFocus
      placeholder
      value
      onChange={ev => onChange(ReactEvent.Form.target(ev)["value"])}
      onKeyDown={ev => {
        let key = ReactEvent.Keyboard.key(ev)
        if preventKeys->Array.includes(key) {
          ReactEvent.Keyboard.preventDefault(ev)
        }
        onKey(key)
        onKeyEvent(ev)
        ReactEvent.Keyboard.stopPropagation(ev)
      }}
    />
}

/// CommonMark comment bodies are presentation only: the original source
/// stays in the view model and the composer. Raw HTML is rendered as text
/// by react-markdown; do not add a raw-HTML plugin or override its safe URL
/// transform. Links with a rejected URL retain their readable label.
module Markdown = {
  module Link = {
    @react.component
    let make = (~href: option<string>=?, ~title: option<string>=?, ~children) =>
      switch href {
      | None | Some("") => <span> children </span>
      | Some(href) =>
        <a
          href
          ?title
          target="_blank"
          rel="noopener noreferrer"
          className="text-accent underline underline-offset-2 focus-visible:outline-2 focus-visible:outline-accent"
          onClick={ev => ReactEvent.Mouse.stopPropagation(ev)}
          onKeyDown={ev => {
            // Native Enter activation must not also run the app keymap's
            // command for the focused thread.
            if ReactEvent.Keyboard.key(ev) == "Enter" {
              ReactEvent.Keyboard.stopPropagation(ev)
            }
          }}
        >
          children
        </a>
      }
  }

  module Render = {
    type components = {a: React.component<Link.props<string, string, React.element>>}

    @module("react-markdown") @react.component
    external make: (~children: string, ~components: components) => React.element = "default"
  }

  @react.component
  let make = (~source: string) =>
    <div
      className="min-w-0 break-words text-sm [&>*+*]:mt-2 [&_p]:whitespace-pre-wrap [&_ul]:list-disc [&_ul]:pl-5 [&_ol]:list-decimal [&_ol]:pl-5 [&_li>ul]:mt-1 [&_li>ol]:mt-1 [&_code]:rounded [&_code]:bg-panel [&_code]:px-1 [&_code]:font-mono [&_code]:text-xs [&_pre]:overflow-x-auto [&_pre]:rounded [&_pre]:bg-panel [&_pre]:p-2 [&_pre]:whitespace-pre [&_pre_code]:p-0 [&_blockquote]:border-l-2 [&_blockquote]:border-border [&_blockquote]:pl-3 [&_blockquote]:text-muted [&_h1]:font-semibold [&_h2]:font-semibold [&_h3]:font-semibold [&_h4]:font-semibold [&_h5]:font-semibold [&_h6]:font-semibold [&_img]:max-w-full"
    >
      <Render components={a: Link.make}> source </Render>
    </div>
}

/// A palette's single result-list tab stop; selection is announced through
/// aria-activedescendant while the list owns focus.
module SearchResults = {
  type kind = Files | Palette | Help
  @react.component
  let make = (~kind, ~label, ~listRef, ~onKey, ~onFocus, ~activeId: option<string>, ~children) => {
    let className = switch kind {
    | Files => "search-hits"
    | Palette => "palette-results"
    | Help => "help-results"
    }
    <div
      className
      role="listbox"
      ariaLabel=label
      ariaActivedescendant=?activeId
      tabIndex={activeId == None ? -1 : 0}
      ref=listRef
      onKeyDown=onKey
      onFocus={_ => onFocus()}
    >
      children
    </div>
  }
}
