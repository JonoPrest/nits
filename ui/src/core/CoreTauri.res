// Tauri adapter (ARCHITECTURE §6.2): `invoke("dispatch", {action})` out,
// `listen("view")` in. The host emits `view` events whose payload is a
// bounded `ViewFrame`; `invoke("attach")` asks it to emit every section.

type event = {payload: JSON.t}

@module("@tauri-apps/api/core")
external invoke: (string, JSON.t) => promise<JSON.t> = "invoke"

@module("@tauri-apps/api/event")
external listen: (string, event => unit) => promise<unit => unit> = "listen"

/// The event the host emits patches on, and the commands it exposes.
let viewEvent = "view"
let dispatchCommand = "dispatch"
let keyCommand = "key"
let attachCommand = "attach"

let errorCommand = "client_error"

/// Console plus the host log (the console is invisible in a packaged app).
let reportError = (message: string) => {
  Console.error(message)
  invoke(
    errorCommand,
    JSON.Encode.object(Dict.fromArray([("message", JSON.Encode.string(message))])),
  )
  ->Promise.then(_ => Promise.resolve())
  ->Promise.catch(_ => Promise.resolve())
  ->ignore
}

let make = (~onError: string => unit=reportError): Core.t => {
  let store = Core.Store.make()
  let decoder = ViewDelivery.make()
  // `listen` registers asynchronously; anything the host emits before it
  // resolves is lost, so `attach` (which makes the host emit every
  // section) waits for it.
  let listening = listen(viewEvent, ev =>
    switch ViewDelivery.acceptJson(decoder, ev.payload) {
    | Applied({kind, patches}) => ViewDelivery.apply(store, kind, patches)
    | Buffered => ()
    | Resync(reason) =>
      onError("view event: " ++ reason)
      invoke(attachCommand, JSON.Encode.object(Dict.make()))
      ->Promise.then(_ => Promise.resolve())
      ->Promise.catch(exn => {
        onError("attach: " ++ Core.message(exn))
        Promise.resolve()
      })
      ->ignore
    }
  )->Promise.catch(exn => {
    onError("listen: " ++ Core.message(exn))
    Promise.resolve(() => ())
  })
  {
    dispatch: action => {
      let args = JSON.Encode.object(Dict.fromArray([("action", Core.actionToJson(action))]))
      invoke(dispatchCommand, args)
      ->Promise.then(_ => Promise.resolve())
      ->Promise.catch(exn => {
        onError("dispatch: " ++ Core.message(exn))
        Promise.resolve()
      })
      ->ignore
    },
    key: chord => {
      let args = JSON.Encode.object(Dict.fromArray([("chord", Keys.toJson(chord))]))
      invoke(keyCommand, args)
      ->Promise.then(_ => Promise.resolve())
      ->Promise.catch(exn => {
        onError("key: " ++ Core.message(exn))
        Promise.resolve()
      })
      ->ignore
    },
    subscribe: listener => Core.Store.subscribe(store, listener),
    attach: () => {
      listening
      ->Promise.then(_ => invoke(attachCommand, JSON.Encode.object(Dict.make())))
      ->Promise.then(_ => Promise.resolve())
      ->Promise.catch(exn => {
        onError("attach: " ++ Core.message(exn))
        Promise.resolve()
      })
      ->ignore
    },
  }
}
