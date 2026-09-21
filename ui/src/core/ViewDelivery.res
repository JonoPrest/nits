// One assembler for browser and Tauri. Observers only receive whole logical
// batches; a sequence gap requires a new snapshot before accepting any delta.
let messageLimit = 64 * 1024
let utf8Length: string => int = %raw(`text => new TextEncoder().encode(text).length`)

type pending = {
  revision: Delivery.ViewRevision.t,
  kind: Delivery.ViewBatchKind.t,
  next: float,
  bytes: float,
  received: float,
  pieces: array<string>,
}
type state =
  | Starting
  | Ready(Delivery.ViewRevision.t)
  | Receiving(pending)
  // Keep the last accepted/assembling revision until a new host generation.
  | AwaitingSnapshot(option<Delivery.ViewRevision.t>)
type t = {mutable state: state}
type outcome =
  | Buffered
  | Applied({kind: Delivery.ViewBatchKind.t, patches: array<View.ViewPatch.t>})
  | Resync(string)

let make = () => {state: Starting}
let reset = decoder => decoder.state = Starting
let beginResync = (decoder, after, reason) => {
  decoder.state = AwaitingSnapshot(after)
  Resync(reason)
}
let fail = (decoder, reason) =>
  switch decoder.state {
  | Starting => beginResync(decoder, None, reason)
  | Ready(revision) => beginResync(decoder, Some(revision), reason)
  | Receiving(pending) => beginResync(decoder, Some(pending.revision), reason)
  | AwaitingSnapshot(_) => Buffered
  }
let isInteger: float => bool = %raw(`Number.isInteger`)
let unsigned = value => isInteger(value) && value >= 0. && value <= 4294967295.
let positive = value => value > 0. && unsigned(value)

let finish = (decoder, revision, kind, patches) => {
  decoder.state = Ready(revision)
  Applied({kind, patches})
}

let start = (decoder, frame: Delivery.ViewFrame.t) =>
  switch frame.body {
  | Complete({patches}) => finish(decoder, frame.revision, frame.kind, patches)
  | Fragment({position: Start({bytes}), json}) => {
      let received = Int.toFloat(utf8Length(json))
      if !positive(bytes) || received == 0. || received >= bytes {
        fail(decoder, "invalid view fragment byte count")
      } else {
        decoder.state = Receiving({
          revision: frame.revision,
          kind: frame.kind,
          next: 1.,
          bytes,
          received,
          pieces: [json],
        })
        Buffered
      }
    }
  | Fragment({position: More(_) | End(_), json: _}) =>
    fail(decoder, "view fragment arrived without its start")
  }

let frame = (decoder, frame: Delivery.ViewFrame.t) => {
  if !unsigned(frame.revision) {
    fail(decoder, "invalid view revision")
  } else {
    switch decoder.state {
    | Starting =>
      switch frame.kind {
      | Delta => Buffered
      | Snapshot => start(decoder, frame)
      }
    | AwaitingSnapshot(after) =>
      switch (frame.kind, after) {
      | (Delta, _) => Buffered
      | (Snapshot, Some(previous)) if frame.revision <= previous => Buffered
      | (Snapshot, _) => start(decoder, frame)
      }
    | Ready(previous) =>
      if frame.revision <= previous {
        fail(decoder, "duplicate or stale view revision")
      } else if frame.kind == Delta && frame.revision != previous +. 1. {
        fail(decoder, "missing view revision")
      } else {
        start(decoder, frame)
      }
    | Receiving(pending) =>
      if frame.revision != pending.revision || frame.kind != pending.kind {
        fail(decoder, "view fragment changed batch identity")
      } else {
        switch frame.body {
        | Complete(_) | Fragment({position: Start(_), json: _}) =>
          fail(decoder, "view batch restarted before completion")
        | Fragment({position, json}) =>
          let (index, final) = switch position {
          | More({index}) => (index, false)
          | End({index}) => (index, true)
          | Start(_) => (0., false)
          }
          let received = pending.received +. Int.toFloat(utf8Length(json))
          if !positive(index) || index != pending.next || json == "" || received > pending.bytes {
            fail(decoder, "invalid or out-of-order view fragment")
          } else if final {
            if received != pending.bytes {
              fail(decoder, "incomplete view batch")
            } else {
              pending.pieces->Array.push(json)
              switch try Ok(JSON.parseOrThrow(pending.pieces->Array.joinWith(""))) catch {
              | exn => Error(Core.message(exn))
              } {
              | Error(reason) => fail(decoder, reason)
              | Ok(json) =>
                switch Core.patchesOfJson(json) {
                | Ok(patches) => finish(decoder, frame.revision, frame.kind, patches)
                | Error(reason) => fail(decoder, reason)
                }
              }
            }
          } else if received >= pending.bytes {
            fail(decoder, "view batch has no final fragment")
          } else {
            pending.pieces->Array.push(json)
            decoder.state = Receiving({...pending, next: index +. 1., received})
            Buffered
          }
        }
      }
    }
  }
}

let acceptJson = (decoder, json) => {
  if utf8Length(JSON.stringify(json)) >= messageLimit {
    fail(decoder, "view message exceeded its byte budget")
  } else {
    switch Core.frameOfJson(json) {
    | Ok(value) => frame(decoder, value)
    | Error(reason) => fail(decoder, reason)
    }
  }
}

let acceptText = (decoder, text) => {
  if utf8Length(text) >= messageLimit {
    fail(decoder, "view message exceeded its byte budget")
  } else {
    switch try Ok(JSON.parseOrThrow(text)) catch {
    | exn => Error(Core.message(exn))
    } {
    | Ok(json) => acceptJson(decoder, json)
    | Error(reason) => fail(decoder, reason)
    }
  }
}

let apply = (store, kind, patches) =>
  switch kind {
  | Delivery.ViewBatchKind.Snapshot => Core.Store.replace(store, patches)
  | Delta => Core.Store.apply(store, patches)
  }
