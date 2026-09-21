open Vitest

let wire = frame => S.reverseConvertToJsonOrThrow(frame, Delivery.ViewFrame.schema)
let send = (decoder, frame) => ViewDelivery.acceptJson(decoder, wire(frame))
let patches = () => [
  Fixtures.parse(View.ViewPatch.schema, "client", "ViewPatch", "Diff"),
  Fixtures.parse(View.ViewPatch.schema, "client", "ViewPatch", "Progress"),
]
let complete = (
  ~revision=1.,
  ~kind=Delivery.ViewBatchKind.Snapshot,
  patches: array<View.ViewPatch.t>,
): Delivery.ViewFrame.t => {
  revision,
  kind,
  body: Delivery.ViewFrameBody.Complete({patches: patches}),
}
let split: (string, int) => array<string> = %raw(`(text, size) => {
  const points = Array.from(text), result = [];
  for (let i = 0; i < points.length; i += size) result.push(points.slice(i, i + size).join(''));
  return result;
}`)
let fragmented = (~revision=1., ~kind=Delivery.ViewBatchKind.Snapshot, ~size=4096, patches) => {
  let encoded = S.reverseConvertToJsonOrThrow(patches, Core.patchesSchema)->JSON.stringify
  let bytes = ViewDelivery.utf8Length(encoded)->Int.toFloat
  let parts = split(encoded, size)
  parts->Array.mapWithIndex((json, index): Delivery.ViewFrame.t => {
    revision,
    kind,
    body: Fragment({
      position: index == 0
        ? Start({bytes: bytes})
        : index == parts->Array.length - 1
        ? End({index: Int.toFloat(index)})
        : More({index: Int.toFloat(index)}),
      json,
    }),
  })
}
let isResync = outcome =>
  switch outcome {
  | ViewDelivery.Resync(_) => true
  | Buffered | Applied(_) => false
  }
let isApplied = outcome =>
  switch outcome {
  | ViewDelivery.Applied(_) => true
  | Buffered | Resync(_) => false
  }

test("fragments preserve Unicode and typed diff metadata with one atomic notification", () => {
  let decoder = ViewDelivery.make()
  let store = Core.Store.make()
  let observed = fn()
  let unsubscribe = Core.Store.subscribe(store, observed)
  let original = patches()->Array.concat([
    View.ViewPatch.Connection({
      connection: Subscribed({}),
      lastError: Some(Internal({message: "λ😀\"\\\r\n"->String.repeat(20000)})),
    }),
  ])
  let frames = fragmented(original)
  expect(frames->Array.length > 2)->toBeTruthy
  frames->Array.forEachWithIndex((frame, index) => {
    expect(
      ViewDelivery.utf8Length(wire(frame)->JSON.stringify) < ViewDelivery.messageLimit,
    )->toBeTruthy
    switch send(decoder, frame) {
    | Applied({kind, patches}) =>
      expect(index)->toBe(frames->Array.length - 1)
      expect(patches)->toEqual(original)
      ViewDelivery.apply(store, kind, patches)
    | Buffered => expect(index < frames->Array.length - 1)->toBeTruthy
    | Resync(reason) => expect(reason)->toBe("no framing error expected")
    }
    expect(observed)->toHaveBeenCalledTimes(index == frames->Array.length - 1 ? 2 : 1)
  })
  expect(store.model.progress.total)->toBe(12)
  unsubscribe()
})

test("missing fragments discard the batch and suppress deltas until a fresh snapshot", () => {
  let decoder = ViewDelivery.make()
  let original = patches()
  let frames = fragmented(~size=80, original)
  expect(send(decoder, frames->Array.getUnsafe(0)))->toEqual(ViewDelivery.Buffered)
  expect(send(decoder, frames->Array.getUnsafe(2))->isResync)->toBeTruthy
  expect(send(decoder, complete(~revision=2., ~kind=Delta, original)))->toEqual(
    ViewDelivery.Buffered,
  )
  expect(send(decoder, frames->Array.getUnsafe(3)))->toEqual(ViewDelivery.Buffered)
  expect(send(decoder, complete(~revision=20., original))->isApplied)->toBeTruthy
  expect(send(decoder, complete(~revision=21., ~kind=Delta, original))->isApplied)->toBeTruthy
  expect(send(decoder, complete(~revision=21., ~kind=Delta, original))->isResync)->toBeTruthy
})

test("duplicate, reordered and cross-revision fragments expose no partial patches", () => {
  let original = patches()
  let frames = fragmented(~size=80, original)
  [
    frames->Array.getUnsafe(0),
    frames->Array.getUnsafe(2),
    {...frames->Array.getUnsafe(1), revision: 2.},
    {...frames->Array.getUnsafe(1), kind: Delta},
  ]->Array.forEach(bad => {
    let decoder = ViewDelivery.make()
    expect(send(decoder, frames->Array.getUnsafe(0)))->toEqual(ViewDelivery.Buffered)
    expect(send(decoder, bad)->isResync)->toBeTruthy
    expect(send(decoder, complete(~revision=10., original))->isApplied)->toBeTruthy
  })
})

test("wrong byte counts and malformed logical JSON request resynchronization", () => {
  [(3., "[", "]"), (2., "[", "x")]->Array.forEach(((bytes, first, last)) => {
    let decoder = ViewDelivery.make()
    let start: Delivery.ViewFrame.t = {
      revision: 1.,
      kind: Snapshot,
      body: Fragment({position: Start({bytes: bytes}), json: first}),
    }
    let end: Delivery.ViewFrame.t = {
      revision: 1.,
      kind: Snapshot,
      body: Fragment({position: End({index: 1.}), json: last}),
    }
    expect(send(decoder, start))->toEqual(ViewDelivery.Buffered)
    expect(send(decoder, end)->isResync)->toBeTruthy
  })
})

test("reconnect resets assembly and snapshots replace stale sections atomically", () => {
  let decoder = ViewDelivery.make()
  let store = Core.Store.make()
  let frames = fragmented(~size=80, patches())
  expect(send(decoder, frames->Array.getUnsafe(0)))->toEqual(ViewDelivery.Buffered)
  ViewDelivery.reset(decoder)
  expect(send(decoder, frames->Array.getUnsafe(1))->isResync)->toBeTruthy
  let initial = [
    View.ViewPatch.Connection({
      connection: Subscribed({}),
      lastError: Some(Internal({message: "old"})),
    }),
  ]
  ViewDelivery.apply(store, Snapshot, initial)
  ViewDelivery.apply(store, Delta, patches())
  expect(store.model.lastError != None)->toBeTruthy
  switch send(decoder, complete(~revision=4., patches())) {
  | Applied({kind, patches}) => ViewDelivery.apply(store, kind, patches)
  | Buffered | Resync(_) => expect(false)->toBeTruthy
  }
  expect(store.model.lastError)->toEqual(None)
  expect(store.model.progress.total)->toBe(12)
})

test("revision comparisons retain the complete unsigned range", () => {
  let decoder = ViewDelivery.make()
  let beforeMax = 4294967294.
  let max = 4294967295.
  expect(send(decoder, complete(~revision=beforeMax, patches()))->isApplied)->toBeTruthy
  expect(send(decoder, complete(~revision=max, ~kind=Delta, patches()))->isApplied)->toBeTruthy
  expect(send(decoder, complete(~revision=0., ~kind=Delta, patches()))->isResync)->toBeTruthy
})

test("over-budget envelopes and malformed wire data fail once per resync", () => {
  let decoder = ViewDelivery.make()
  let huge = [
    View.ViewPatch.Connection({
      connection: Subscribed({}),
      lastError: Some(Internal({message: "x"->String.repeat(ViewDelivery.messageLimit)})),
    }),
  ]
  expect(send(decoder, complete(huge))->isResync)->toBeTruthy
  expect(ViewDelivery.acceptText(decoder, "invalid JSON"))->toEqual(ViewDelivery.Buffered)
  expect(send(decoder, complete(~revision=4., patches()))->isApplied)->toBeTruthy
})

test("resync retains its revision floor for stale complete and fragmented snapshots", () => {
  let decoder = ViewDelivery.make()
  let original = patches()
  expect(send(decoder, complete(~revision=10., original))->isApplied)->toBeTruthy
  expect(send(decoder, complete(~revision=10., original))->isResync)->toBeTruthy
  expect(send(decoder, complete(~revision=9., original)))->toEqual(ViewDelivery.Buffered)
  expect(send(decoder, complete(~revision=10., original)))->toEqual(ViewDelivery.Buffered)
  fragmented(~revision=9., ~size=80, original)->Array.forEach(frame =>
    expect(send(decoder, frame))->toEqual(ViewDelivery.Buffered)
  )
  expect(send(decoder, complete(~revision=11., original))->isApplied)->toBeTruthy

  // Losing a newer in-progress snapshot must not permit an older snapshot to
  // replace the model. Its start established the next batch's identity.
  let newer = fragmented(~revision=20., ~size=80, original)
  expect(send(decoder, newer->Array.getUnsafe(0)))->toEqual(ViewDelivery.Buffered)
  expect(send(decoder, newer->Array.getUnsafe(2))->isResync)->toBeTruthy
  expect(send(decoder, complete(~revision=19., original)))->toEqual(ViewDelivery.Buffered)
  newer->Array.forEach(frame => expect(send(decoder, frame))->toEqual(ViewDelivery.Buffered))
  let fresh = fragmented(~revision=21., ~size=80, original)
  fresh->Array.forEachWithIndex((frame, index) => {
    let result = send(decoder, frame)
    expect(isApplied(result))->toBe(index == fresh->Array.length - 1)
    expect(isResync(result))->toBe(false)
  })
  // A replacement host really does start a fresh revision sequence.
  ViewDelivery.reset(decoder)
  expect(send(decoder, complete(~revision=1., original))->isApplied)->toBeTruthy
})
