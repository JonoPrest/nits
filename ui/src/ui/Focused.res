// Scroll the element carrying `data-focused` into view (§6.4: focus is
// core state, the DOM follows it).

type element
@val @scope("document") external querySelector: string => Nullable.t<element> = "querySelector"
@send external scrollElement: (element, {"block": string}) => unit = "scrollIntoView"

let scrollIntoView = () =>
  switch querySelector("[data-focused]")->Nullable.toOption {
  | Some(el) => el->scrollElement({"block": "nearest"})
  | None => ()
  }

@val @scope("document") external getElementById: string => Nullable.t<element> = "getElementById"

/// IDs are opaque strings; avoid building a CSS selector from route input.
let comment = (id: string) =>
  switch getElementById("comment-" ++ id)->Nullable.toOption {
  | Some(el) => el->scrollElement({"block": "center"})
  | None => ()
  }
