// Merged file tree (§5.5, §5.6) from `View.treeView`; every node in
// display order gets its visible index so focus can name it. Rows show
// lines added/removed and a thread-count badge (UI-DESIGN §Layout);
// viewed files dim with a check mark (`v` toggles viewed).

open View

/// Flatten visible nodes in display order, with depth.
let rec flatten = (nodes: array<TreeNode.t>, depth: int, out: array<(TreeNode.t, int)>) =>
  nodes->Array.forEach(n => {
    out->Array.push((n, depth))
    switch n {
    | Dir({expanded: true, children}) => flatten(children, depth + 1, out)
    | Dir(_) | File(_) => ()
    }
  })

@react.component
let make = (
  ~tree: TreeView.t,
  ~focus: Focus.t,
  ~home: option<string>=?,
  ~chrome: array<Hint.t>=[],
  ~dispatch: Action.t => unit,
) => {
  let rows = []
  flatten(tree.roots, 0, rows)
  let focusedIndex = switch focus {
  | Tree({index}) => Some(index)
  | _ => None
  }
  // A stable header: the workspace name as a home button (back to the
  // review list), never the open file's breadcrumbs.
  <nav className="tree panel" ariaLabel="files">
    <header className="panel-header tree-header">
      {switch home {
      | Some(name) =>
        <button
          type_="button"
          className="tree-home"
          title=?{Chrome.tip(chrome, GoHome)}
          onClick={_ => dispatch(GoHome({}))}
        >
          {React.string("⌂ " ++ name)}
        </button>
      | None => React.string("Files")
      }}
    </header>
    <ul className="tree-list" role="tree">
      {rows
      ->Array.mapWithIndex(((node, depth), i) => {
        let focused = focusedIndex == Some(i)
        let style: ReactDOM.Style.t = {paddingLeft: Int.toString(depth * 12 + 4) ++ "px"}
        let onSelect = _ => dispatch(SetFocus({focus: Focus.Tree({index: i})}))
        let item = switch node {
        | Dir({name, repoId, path, expanded, changedBelow}) =>
          <li
            key={Int.toString(i)}
            className="tree-dir"
            role="treeitem"
            style
            onClick={ev => {
              onSelect(ev)
              dispatch(ToggleDir({repoId, path}))
            }}
          >
            <span className="tree-glyph"> {React.string(expanded ? "▾" : "▸")} </span>
            <span className="tree-name"> {React.string(name)} </span>
            {changedBelow > 0
              ? <span className="tree-badge"> {React.string(Int.toString(changedBelow))} </span>
              : React.null}
          </li>
        | File({name, viewed, open_, additions, deletions, threads}) =>
          <li
            key={Int.toString(i)}
            className={"tree-file" ++
            (open_ ? " tree-open" : "") ++ (viewed == Viewed ? " tree-viewed-done" : "")}
            role="treeitem"
            style
            onClick={ev => {
              onSelect(ev)
              // The mouse alias of `enter`: the same command, so the two
              // cannot drift (an already-open file keeps its window).
              dispatch(RunCommand({command: Open}))
            }}
          >
            <span className="tree-glyph" />
            <span className="tree-name"> {React.string(name)} </span>
            {viewed == Viewed
              ? <span className="tree-viewed-mark" title="viewed"> {React.string("✓")} </span>
              : React.null}
            {threads > 0
              ? <span className="tree-threads" title={Int.toString(threads) ++ " thread(s)"}>
                  {React.string(Int.toString(threads))}
                </span>
              : React.null}
            {switch (additions, deletions) {
            | (Some(a), Some(d)) =>
              <>
                <span className="tree-stat-add"> {React.string("+" ++ Int.toString(a))} </span>
                <span className="tree-stat-del"> {React.string("−" ++ Int.toString(d))} </span>
              </>
            | _ => React.null
            }}
          </li>
        }
        Attrs.focused(item, focused)
      })
      ->React.array}
    </ul>
  </nav>
}
