// One root or reply's immutable suggestion, shared by inline and conversation views.
open View

let author = (value: Domain.Author.t) =>
  switch value {
  | Human({name}) => name
  | Agent({name}) => name ++ " (agent)"
  | Daemon(_) => "nitsd"
  }
let statusText = (status: SuggestionStatus.t) =>
  switch status {
  | Unloaded(_) => "Preview available"
  | Loading(_) => "Checking suggestion…"
  | Ready(_) => "Ready to apply"
  | Stale(_) => "Stale · file has changed"
  | Rejected(_) => "Cannot apply"
  | Applying(_) => "Applying…"
  | Uncertain(_) => "Application unconfirmed"
  | Applied(_) => "Applied"
  }

module Patch = {
  @react.component
  let make = (~hunks: array<Suggestion.Hunk.t>) =>
    <div className="suggestion-patch">
      <table className="suggestion-diff" ariaLabel="Suggested patch">
        <thead>
          <tr>
            <th scope="col"> {React.string("Old")} </th>
            <th scope="col"> {React.string("New")} </th>
            <th scope="col"> {React.string("Proposed changes")} </th>
          </tr>
        </thead>
        {hunks
        ->Array.mapWithIndex((hunk, index) =>
          <tbody key={Int.toString(index)}>
            <tr>
              <th colSpan=3 className="suggestion-hunk"> {React.string(hunk.header)} </th>
            </tr>
            {hunk.lines
            ->Array.mapWithIndex((line, index) => {
              let (old, next, prefix, kind) = switch line.kind {
              | Context({old, new}) => (Int.toString(old), Int.toString(new), " ", "context")
              | Remove({old}) => (Int.toString(old), "", "−", "remove")
              | Add({new}) => ("", Int.toString(new), "+", "add")
              }
              <tr key={Int.toString(index)} className={"suggestion-line suggestion-" ++ kind}>
                <td className="suggestion-number"> {React.string(old)} </td>
                <td className="suggestion-number"> {React.string(next)} </td>
                <td>
                  <div className="suggestion-source">
                    <span className="suggestion-prefix" ariaLabel=kind>
                      {React.string(prefix)}
                    </span>
                    <code> {Row.sourceText(line.text)} </code>
                  </div>
                  <span className="suggestion-ending">
                    {React.string(Row.endingName(line.ending))}
                  </span>
                </td>
              </tr>
            })
            ->React.array}
          </tbody>
        )
        ->React.array}
      </table>
    </div>
}

@react.component
let make = (
  ~suggestion: SuggestionView.t,
  ~repositories: RepositoryIdentity.context=Unavailable,
  ~chrome: array<Hint.t>,
  ~dispatch: Action.t => unit,
) => {
  let record = suggestion.record
  let file: option<FileRef.t> = switch record.anchor {
  | File({repoId, path}) | Lines({repoId, path}) => Some({repoId, path})
  | Review(_) => None
  }
  let location = switch file {
  | Some(file) => RepositoryIdentity.fileDescription(repositories, file)
  | None => "review"
  }
  let ready = switch suggestion.status {
  | Ready(_) => true
  | _ => false
  }
  let busy = switch suggestion.status {
  | Loading(_) | Applying(_) => true
  | _ => false
  }
  <div onClick={ev => ReactEvent.Mouse.stopPropagation(ev)}>
    <UI.Panel title="Suggested change" ariaLabel={"Suggested change for " ++ location}>
      <UI.Box gap=Sm>
        {switch file {
        | Some(file) => <RepositoryIdentity.File repositories file />
        | None => <UI.Badge text="Review-level suggestion" />
        }}
        {switch record.anchor {
        | File({blobOid}) | Lines({blobOid}) =>
          <span title=blobOid>
            {React.string("Original blob " ++ String.slice(blobOid, ~start=0, ~end=12))}
          </span>
        | Review(_) => React.null
        }}
        <span role="status">
          <UI.Badge text={statusText(suggestion.status)} />
        </span>
        {switch suggestion.inspection {
        | Some(Checked({hunks})) => <Patch hunks />
        | Some(Rejected(_)) | None =>
          <pre className="suggestion-raw" ariaLabel="Suggested patch (raw)">
            <code> {React.string(record.patch)} </code>
          </pre>
        }}
        {switch suggestion.status {
        | Rejected({message}) | Uncertain({message}) =>
          <p role="alert"> {React.string(message)} </p>
        | Stale(_) =>
          <p>
            {React.string(
              "The checkout no longer matches the original blob. The preview is preserved; check again after resolving the change.",
            )}
          </p>
        | Ready(_) =>
          <p>
            {React.string(
              "Checked against the original file. Apply checks it again before writing.",
            )}
          </p>
        | Unloaded(_) | Loading(_) | Applying(_) | Applied(_) => React.null
        }}
        {switch suggestion.notice {
        | Some(message) => <p role="alert"> {React.string(message)} </p>
        | None => React.null
        }}
        {switch record.outcome {
        | Applied({receipt}) =>
          <UI.Box gap=Xs>
            <p title={Stepper.absolute(receipt.at)}>
              {React.string(
                "Applied by " ++ author(receipt.author) ++ " · " ++ Stepper.relative(receipt.at),
              )}
            </p>
            <RepositoryIdentity.File
              repositories file={{repoId: receipt.repoId, path: receipt.path}}
            />
            <span title=receipt.resultBlob>
              {React.string("Result blob " ++ String.slice(receipt.resultBlob, ~start=0, ~end=12))}
            </span>
          </UI.Box>
        | Unapplied(_) => React.null
        }}
        <UI.Box direction=Row gap=Sm>
          <UI.Button
            label="Preview suggestion"
            ariaLabel={"Preview suggestion for " ++ location}
            title=?{Chrome.tip(chrome, PreviewSuggestion)}
            disabled=busy
            onClick={() => dispatch(PreviewSuggestion({commentId: record.commentId}))}
          />
          <UI.Button
            label="Apply suggestion"
            ariaLabel={"Apply suggestion for " ++ location}
            kind=Primary
            title=?{Chrome.tip(chrome, ApplySuggestion)}
            disabled={!ready}
            onClick={() => dispatch(ApplySuggestion({commentId: record.commentId}))}
          />
        </UI.Box>
      </UI.Box>
    </UI.Panel>
  </div>
}
