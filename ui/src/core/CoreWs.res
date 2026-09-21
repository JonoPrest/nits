// Browser adapter: the same contract as CoreTauri, over a WebSocket to
// `nits-web` (crates/nits-client-web). Commands go out as
// `{"cmd":"dispatch","action":…}` etc.; patch batches come back as JSON
// delivery frames. Sends queue until the socket opens; `attach` catches the client
// up, and a dropped connection retries with a fresh attach.

module Ws = {
  type t
  @new external make: string => t = "WebSocket"
  @send external send: (t, string) => unit = "send"
  @set external onopen: (t, unit => unit) => unit = "onopen"
  @set external onclose: (t, unit => unit) => unit = "onclose"
  @set external onerror: (t, unit => unit) => unit = "onerror"
  @set external onmessage: (t, {"data": string} => unit) => unit = "onmessage"
}

@val external setTimeout: (unit => unit, int) => unit = "setTimeout"

let retryMs = 1000

let make = (~url: string, ~onError: string => unit=e => Console.error(e)): Core.t => {
  let store = Core.Store.make()
  let decoder = ViewDelivery.make()
  let socket: ref<option<Ws.t>> = ref(None)
  let open_ = ref(false)
  let queue: ref<array<string>> = ref([])
  // A stale socket may report close after its successor exists. Only the
  // current generation may reset the store or schedule another connection.
  let generation = ref(0)
  let recovery = CreationRecovery.make()
  let recovering = ref(false)
  let restoreSent = ref(false)
  let keyPrefix = ref("")
  let send = (text: string) =>
    switch (socket.contents, open_.contents) {
    | (Some(ws), true) => Ws.send(ws, text)
    | _ => queue.contents->Array.push(text)
    }
  let command = (fields: array<(string, JSON.t)>) =>
    send(JSON.stringify(JSON.Encode.object(Dict.fromArray(fields))))
  let attach = () => command([("cmd", JSON.Encode.string("attach"))])
  let rec connect = () => {
    generation := generation.contents + 1
    let mine = generation.contents
    ViewDelivery.reset(decoder)
    let ws = Ws.make(url)
    socket := Some(ws)
    open_ := false
    Ws.onopen(ws, () => {
      if mine == generation.contents {
        open_ := true
        // Attach first so the full model precedes any queued command's patches.
        Ws.send(
          ws,
          JSON.stringify(
            JSON.Encode.object(Dict.fromArray([("cmd", JSON.Encode.string("attach"))])),
          ),
        )
        let pending = queue.contents
        queue := []
        pending->Array.forEach(text => Ws.send(ws, text))
      }
    })
    Ws.onmessage(ws, ev => {
      if mine == generation.contents && open_.contents {
        switch ViewDelivery.acceptText(decoder, ev["data"]) {
        | Applied({kind, patches}) =>
          ViewDelivery.apply(store, kind, patches)
          if recovering.contents {
            switch (recovery.snapshot, store.model.home.creating) {
            | (Some(saved), Some(creation)) if saved.creation.reviewId == creation.reviewId =>
              recovering := false
              restoreSent := false
              CreationRecovery.observe(recovery, Some(creation))
            | (Some(saved), _) =>
              switch store.model.connection {
              | Subscribed(_) =>
                if !restoreSent.contents {
                  if saved.creation.context == store.model.daemonContext {
                    restoreSent := true
                    command([
                      ("cmd", JSON.Encode.string("dispatch")),
                      (
                        "action",
                        Core.actionToJson(
                          RestoreReviewCreation({creation: saved.creation, resume: saved.resume}),
                        ),
                      ),
                    ])
                  } else {
                    onError(
                      "The retained review belongs to a different daemon context and was not restored.",
                    )
                    CreationRecovery.observe(recovery, None)
                    recovering := false
                  }
                }
              | Disconnected(_) | Connecting(_) | Rejected(_) => ()
              }
            | (None, _) => recovering := false
            }
          } else {
            CreationRecovery.observe(recovery, store.model.home.creating)
          }
        | Buffered => ()
        | Resync(reason) =>
          onError("view message: " ++ reason)
          attach()
        }
      }
    })
    Ws.onclose(ws, () => {
      if mine == generation.contents {
        if open_.contents {
          onError("nits-web connection lost; retrying")
        }
        open_ := false
        socket := None
        recovering := recovery.snapshot != None
        restoreSent := false
        keyPrefix := ""
        ViewDelivery.reset(decoder)
        Core.Store.reset(store)
        setTimeout(connect, retryMs)
      }
    })
    Ws.onerror(ws, () => ())
  }
  connect()
  {
    dispatch: action => {
      let creationIntent = switch action {
      | RunCommand({command: Submit}) if store.model.openReview != None => false
      | _ => CreationRecovery.beforeAction(recovery, action, ~workspaces=store.model.workspaces)
      }
      if creationIntent && (!open_.contents || recovering.contents) {
        onError(
          "The connection is recovering. Your review inputs are retained; wait before retrying.",
        )
      } else {
        command([("cmd", JSON.Encode.string("dispatch")), ("action", Core.actionToJson(action))])
      }
    },
    key: chord => {
      let text = keyPrefix.contents ++ Keys.text(chord)
      switch store.model.bindings->Array.find(h => h.keys == text) {
      | Some(hint) =>
        keyPrefix := ""
        if store.model.openReview == None {
          let _ = CreationRecovery.beforeAction(
            recovery,
            RunCommand({command: hint.command}),
            ~workspaces=store.model.workspaces,
          )
        }
      | None =>
        keyPrefix := (
            store.model.bindings->Array.some(h => h.keys->String.startsWith(text ++ " "))
              ? text ++ " "
              : ""
          )
      }
      if open_.contents && !recovering.contents {
        command([("cmd", JSON.Encode.string("key")), ("chord", Keys.toJson(chord))])
      }
    },
    subscribe: listener => Core.Store.subscribe(store, listener),
    attach,
  }
}

/// `?ws=<url>` beats same-origin `/ws` (which `nits` and the Vite dev
/// proxy both serve).
let defaultUrl = () => {
  let fromQuery = %raw(`new URLSearchParams(window.location.search).get("ws")`)
  switch fromQuery->Nullable.toOption {
  | Some(url) => url
  | None =>
    %raw(`(window.location.protocol === "https:" ? "wss://" : "ws://") + window.location.host + "/ws"`)
  }
}

/// `?review=<id>`: the review to open once subscribed (bare `nits` links
/// here).
let reviewParam = (): option<string> =>
  %raw(`new URLSearchParams(window.location.search).get("review")`)->Nullable.toOption

/// The portable route takes precedence over the legacy review-only parameter.
let referenceParam = (): option<string> =>
  %raw(`new URLSearchParams(window.location.search).get("reference")`)->Nullable.toOption
