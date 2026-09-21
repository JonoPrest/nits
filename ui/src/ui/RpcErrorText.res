// Exhaustive shell presentation: daemon failures must never disappear.
let message = (error: Rpc.RpcError.t) =>
  switch error {
  | NotFound({id}) => "Not found: " ++ id
  | Invalid({reason}) => reason
  | Forbidden({reason}) => "Permission denied: " ++ reason
  | SeqTooOld({oldest}) => "Event history changed. Reconnect from " ++ Float.toString(oldest) ++ "."
  | Cancelled(_) => "The operation was cancelled."
  | UnsupportedProtocol({requested, supported}) =>
    "Unsupported protocol " ++ requested ++ ". Supported: " ++ supported->Array.join(", ")
  | VersionMismatch({negotiated, received}) =>
    "Protocol mismatch: expected " ++
    negotiated ++
    ", received " ++
    received ++ ". Reconnect to continue."
  | Internal({message}) => message
  }
