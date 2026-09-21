// The UI's only door to the core (ARCHITECTURE §6.2): dispatch actions,
// receive the view. Adapters implement `t`; components never see IPC.

type unsubscribe = unit => unit

type t = {
  /// Send a user intent to the core.
  dispatch: Action.t => unit,
  /// Send a key chord (outside text inputs) for the core's keymap.
  key: Keys.KeyChord.t => unit,
  /// Called with the current model now and after every patch.
  subscribe: (View.ViewModel.t => unit) => unsubscribe,
  /// Ask the host for every section (a UI that just attached).
  attach: unit => unit,
}

/// The UI-side copy of the model: applies patches, fans out changes.
module Store = {
  type t = {
    mutable model: View.ViewModel.t,
    mutable listeners: array<View.ViewModel.t => unit>,
  }

  let make = (): t => {model: View.ViewModel.empty, listeners: []}

  let apply = (store: t, patches: array<View.ViewPatch.t>) => {
    store.model = patches->Array.reduce(store.model, View.ViewPatch.apply)
    store.listeners->Array.forEach(l => l(store.model))
  }

  /// A complete snapshot replaces all sections in one observer notification.
  let replace = (store: t, patches: array<View.ViewPatch.t>) => {
    store.model = patches->Array.reduce(View.ViewModel.empty, View.ViewPatch.apply)
    store.listeners->Array.forEach(l => l(store.model))
  }

  /// A browser WebSocket reconnects to a fresh host/core. Clear the old
  /// session before its replacement attaches so key sequence numbers and
  /// navigational state cannot cross that boundary.
  let reset = (store: t) => {
    store.model = View.ViewModel.empty
    store.listeners->Array.forEach(l => l(store.model))
  }

  let subscribe = (store: t, listener: View.ViewModel.t => unit): unsubscribe => {
    store.listeners = store.listeners->Array.concat([listener])
    listener(store.model)
    () => {
      store.listeners = store.listeners->Array.filter(l => l !== listener)
    }
  }
}

let message = (exn: exn): string =>
  JsExn.fromException(exn)->Option.flatMap(JsExn.message)->Option.getOr("invalid")

// JSON crossings, so adapters and tests never touch Sury directly.
let actionToJson = (action: Action.t): JSON.t =>
  S.reverseConvertToJsonOrThrow(action, Action.schema)

let actionOfJson = (json: JSON.t): result<Action.t, string> =>
  try {
    Ok(S.parseJsonOrThrow(json, Action.schema))
  } catch {
  | exn => Error(message(exn))
  }

let patchesSchema = S.array(View.ViewPatch.schema)

let patchesOfJson = (json: JSON.t): result<array<View.ViewPatch.t>, string> =>
  try {
    Ok(S.parseJsonOrThrow(json, patchesSchema))
  } catch {
  | exn => Error(message(exn))
  }

let modelToJson = (model: View.ViewModel.t): JSON.t =>
  S.reverseConvertToJsonOrThrow(model, View.ViewModel.schema)

let frameOfJson = (json: JSON.t): result<Delivery.ViewFrame.t, string> =>
  try {
    Ok(S.parseJsonOrThrow(json, Delivery.ViewFrame.schema))
  } catch {
  | exn => Error(message(exn))
  }
