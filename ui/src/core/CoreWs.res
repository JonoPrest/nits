// Browser adapter: the same contract as CoreTauri, over a WebSocket to
// `nits-web` (crates/nits-client-web). Commands go out as
// `{"cmd":"dispatch","action":…}` etc.; patch batches come back as JSON
// arrays. Sends queue until the socket opens; `attach` catches the client
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
  let socket: ref<option<Ws.t>> = ref(None)
  let open_ = ref(false)
  let queue: ref<array<string>> = ref([])
  // A stale socket may report close after its successor exists. Only the
  // current generation may reset the store or schedule another connection.
  let generation = ref(0)
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
      if mine == generation.contents {
        switch try Ok(JSON.parseOrThrow(ev["data"])) catch {
        | exn => Error(Core.message(exn))
        } {
        | Ok(json) =>
          switch Core.patchesOfJson(json) {
          | Ok(patches) => Core.Store.apply(store, patches)
          | Error(e) => onError("view message: " ++ e)
          }
        | Error(e) => onError("view message: " ++ e)
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
        Core.Store.reset(store)
        setTimeout(connect, retryMs)
      }
    })
    Ws.onerror(ws, () => ())
  }
  connect()
  {
    dispatch: action =>
      command([("cmd", JSON.Encode.string("dispatch")), ("action", Core.actionToJson(action))]),
    key: chord => command([("cmd", JSON.Encode.string("key")), ("chord", Keys.toJson(chord))]),
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
