// Resolve a configured chord sequence synchronously, before a host round trip.
type t = {mutable keys: array<string>}

let make = (): t => {keys: []}

/// What the core will make of `chord`, resolved against the same
/// bindings it uses: a command, the start of a longer one, or nothing
/// at all. The shell still counts every chord sent to the core; this
/// prediction only controls native defaults and synchronous copy gestures.
type outcome =
  | Runs(View.Command.t)
  | Prefix
  | Unbound

let step = (p: t, bindings: array<View.Hint.t>, chord: Keys.KeyChord.t): outcome => {
  let typed = Array.concat(p.keys, [Keys.text(chord)])
  let text = typed->Array.join(" ")
  let exact = bindings->Array.find(h => h.keys == text)
  let prefix = bindings->Array.some(h => h.keys->String.startsWith(text ++ " "))
  switch (exact, prefix) {
  | (Some(h), _) => {
      p.keys = []
      Runs(h.command)
    }
  | (None, true) => {
      p.keys = typed
      Prefix
    }
  | (None, false) => {
      // Cancelling a sequence consumes the key rather than triggering
      // a browser action as an accidental fallback.
      let cancelled = Array.length(p.keys) > 0
      p.keys = []
      cancelled ? Prefix : Unbound
    }
  }
}
