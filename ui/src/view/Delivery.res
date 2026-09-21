@@warning("-27")

// Rust uses unsigned 32-bit values. ReScript int schemas accept only signed
// 32-bit integers; float preserves the full range exactly. ViewDelivery checks
// integer/range invariants at the boundary before using these values.
module ViewRevision = {
  @schema type t = float
}
module ViewFragmentIndex = {
  @schema type t = float
}
module ViewBatchBytes = {
  @schema type t = float
}
module ViewBatchKind = {
  @schema type t = Snapshot | Delta
}
module ViewFragmentPosition = {
  @schema @tag("type")
  type t =
    | Start({bytes: ViewBatchBytes.t})
    | More({index: ViewFragmentIndex.t})
    | End({index: ViewFragmentIndex.t})
}
module ViewFrameBody = {
  @schema @tag("type")
  type t =
    | Complete({patches: array<View.ViewPatch.t>})
    | Fragment({position: ViewFragmentPosition.t, json: string})
}
module ViewFrame = {
  @schema
  type t = {
    revision: ViewRevision.t,
    kind: ViewBatchKind.t,
    body: ViewFrameBody.t,
  }
}
