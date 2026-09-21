open Vitest
open TestingLibrary

afterEach(cleanup)

let build: Lifecycle.BuildDescriptor.t = {
  digest: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  release: {channel: "stable", version: "1.2.0"},
  protocol: "0.18.0",
  schema: 10,
  control: 1,
  worker: 1,
}
let operation: Lifecycle.UpgradeOperation.t = {
  id: "01AAAAAAAAAAAAAAAAAAAAAAAA",
  source: {...build, release: {...build.release, version: "1.1.0"}},
  target: build,
  progress: Active({stage: Draining}),
}
let chrome: array<View.Hint.t> = [
  {command: InspectDaemon, keys: "g D", label: "daemon status"},
  {command: UpgradeDaemon, keys: "g U", label: "activate installed version"},
]

test("management controls use the core commands and configured tooltips", () => {
  let dispatch = fn()
  let {rerender} = render(<DaemonStatus management={Idle({})} chrome dispatch />)
  FireEvent.click(Screen.getByText("Daemon status"))
  expect(dispatch)->toHaveBeenLastCalledWith(Action.RunCommand({command: InspectDaemon}))
  FireEvent.click(Screen.getByText("Activate installed version"))
  expect(dispatch)->toHaveBeenLastCalledWith(Action.RunCommand({command: UpgradeDaemon}))
  expect(Element.getAttribute(Screen.getByText("Daemon status"), "title"))->toEqual(
    Nullable.make("daemon status (g D)"),
  )
  rerender(<DaemonStatus management={Upgrading({})} chrome dispatch />)
  FireEvent.click(Screen.getByText("Activate installed version"))
  expect(dispatch)->toHaveBeenCalledTimes(2)
})

test("accepted is distinct from readiness and reports the operation and phase", () => {
  let dispatch = fn()
  let {container, rerender} = render(
    <DaemonStatus
      management={Outcome({result: Accepted({operation: operation})})} chrome dispatch
    />,
  )
  expect(Element.textContent(container))->toContain("finishing accepted work")
  expect(Element.textContent(container))->toContain(operation.id)
  expect(Element.textContent(container))->toContain("Check status for readiness")
  rerender(
    <DaemonStatus
      management={Outcome({result: Restarted({operation: operation})})} chrome dispatch
    />,
  )
  expect(Element.textContent(container))->toContain("Now running 1.2.0 (aaaaaaaaaaaa)")
})

test("status distinguishes running and installed builds and unmanaged failures", () => {
  let dispatch = fn()
  let status: Lifecycle.ManagedDaemonStatus.t = {
    running: Running({build: operation.source}),
    installed: Available({build: build}),
    operation: Some(operation),
  }
  let {container, rerender} = render(
    <DaemonStatus management={Status({status: status})} chrome dispatch />,
  )
  expect(Element.textContent(container))->toContain("Running 1.1.0")
  expect(Element.textContent(container))->toContain("Installed 1.2.0")
  rerender(
    <DaemonStatus
      management={Status({status: {...status, running: NotManaged({}), operation: None}})}
      chrome
      dispatch
    />,
  )
  expect(Element.textContent(container))->toContain("managed elsewhere")
  rerender(
    <DaemonStatus
      management={Outcome({
        result: Failed({
          failure: {
            stage: Draining,
            kind: DrainTimeout,
            message: "Accepted work still holds the store; no replacement was started",
          },
        }),
      })}
      chrome
      dispatch
    />,
  )
  expect(Element.textContent(container))->toContain("no replacement was started")
})
