// 4.2 adapter tests with a mocked Tauri API: actions go out as
// `invoke("dispatch", {action})`, `view` events apply patches to the store,
// subscribers see every change, and no IPC message exceeds 64 KB across a
// scripted session (every fixture patch, a long comment).

import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.fn(async () => null);
type Handler = (ev: { payload: unknown }) => void;
const handlers: Record<string, Handler[]> = {};
const listen = vi.fn(async (name: string, handler: Handler) => {
  (handlers[name] ??= []).push(handler);
  return () => {
    handlers[name] = handlers[name].filter((h) => h !== handler);
  };
});
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({ listen }));

// Compiled by `pnpm rescript` before the tests run.
const CoreTauri = await import("../src/core/CoreTauri.res.mjs");
const Core = await import("../src/core/Core.res.mjs");
const CoreWasm = await import("../src/core/CoreWasm.res.mjs");
const Keys = await import("../src/view/Keys.res.mjs");

const fixtures = join(__dirname, "..", "..", "fixtures", "client");
const patchFixtures = () =>
  readdirSync(join(fixtures, "ViewPatch"))
    .filter((f) => f.endsWith(".json"))
    .map((f) => JSON.parse(readFileSync(join(fixtures, "ViewPatch", f), "utf8")));
const actionFixture = (name: string) =>
  JSON.parse(readFileSync(join(fixtures, "Action", `${name}.json`), "utf8"));

const IPC_LIMIT = 64 * 1024;
const bytes = (v: unknown) => new TextEncoder().encode(JSON.stringify(v)).length;

let revision = 0;
const emit = (payload: unknown) => {
  if (Array.isArray(payload)) {
    revision += 1;
    payload = {revision, kind: revision === 1 ? "Snapshot" : "Delta", body: {type: "Complete", patches: payload}};
  }
  for (const h of handlers["view"] ?? []) h({ payload });
};

const tick = () => new Promise((r) => setTimeout(r, 0));

beforeEach(() => {
  revision = 0;
  invoke.mockClear();
  listen.mockClear();
  for (const k of Object.keys(handlers)) delete handlers[k];
});

describe("CoreTauri", () => {
  it("listens for view events and applies patches to subscribers", async () => {
    const errors: string[] = [];
    const core = CoreTauri.make((e: string) => errors.push(e));
    await tick();
    expect(listen).toHaveBeenCalledWith("view", expect.any(Function));
    const seen: unknown[] = [];
    const unsubscribe = core.subscribe((m: unknown) => seen.push(m));
    // Subscribing delivers the empty model at once.
    expect(seen.length).toBe(1);
    emit(patchFixtures());
    expect(seen.length).toBe(2);
    const model = seen[1] as { progress: { total: number }; focus: { type: string } };
    expect(model.progress.total).toBe(12);
    expect(model.focus.type).toBe("Diff");
    expect(errors).toEqual([]);
    unsubscribe();
    emit(patchFixtures());
    expect(seen.length).toBe(2);
    // A malformed event is reported, not thrown.
    emit([{ type: "Nope" }]);
    expect(errors.length).toBe(1);
  });

  it("dispatches actions as invoke(dispatch, {action}) and attaches", async () => {
    const core = CoreTauri.make();
    await tick();
    const action = Core.actionOfJson(actionFixture("Viewport"));
    expect(action.TAG).toBe("Ok");
    core.dispatch(action._0);
    await tick();
    expect(invoke).toHaveBeenCalledWith("dispatch", { action: actionFixture("Viewport") });
    core.attach();
    await tick();
    expect(invoke).toHaveBeenCalledWith("attach", {});
  });

  it("attaches only after the view listener is registered", async () => {
    let resolveListen: (v: () => void) => void = () => {};
    listen.mockImplementationOnce(() => new Promise<() => void>((r) => (resolveListen = r)));
    const core = CoreTauri.make();
    core.attach();
    await tick();
    expect(invoke).not.toHaveBeenCalledWith("attach", {});
    resolveListen(() => {});
    await tick();
    expect(invoke).toHaveBeenCalledWith("attach", {});
  });

  it("sends key chords as invoke(key, {chord}) in the fixture shape", async () => {
    const core = CoreTauri.make();
    await tick();
    const chord = Keys.ofBrowser({ key: "p", ctrlKey: true, altKey: false, shiftKey: false, metaKey: false });
    expect(chord).toBeDefined();
    core.key(chord);
    await tick();
    const expected = JSON.parse(readFileSync(join(fixtures, "KeyChord", "default.json"), "utf8"));
    expect(invoke).toHaveBeenCalledWith("key", { chord: expected });
    // Named keys keep shift; printable ones imply it; modifiers alone are nothing.
    const enter = Keys.ofBrowser({ key: "Enter", ctrlKey: false, altKey: false, shiftKey: true, metaKey: false });
    expect(Keys.toJson(enter)).toEqual({ key: { type: "Named", key: "Enter" }, mods: { ctrl: false, alt: false, shift: true, meta: false } });
    const upper = Keys.ofBrowser({ key: "G", ctrlKey: false, altKey: false, shiftKey: true, metaKey: false });
    expect(Keys.toJson(upper)).toEqual({ key: { type: "Char", c: "G" }, mods: { ctrl: false, alt: false, shift: false, meta: false } });
    expect(Keys.ofBrowser({ key: "Shift", ctrlKey: false, altKey: false, shiftKey: true, metaKey: false })).toBeUndefined();
  });

  it("keeps every IPC message under 64 KB in a scripted session", async () => {
    const core = CoreTauri.make();
    await tick();
    // Measure the actual envelope, including the native event wrapper.
    for (const patch of patchFixtures()) {
      const frame = {revision: 1, kind: "Snapshot", body: {type: "Complete", patches: [patch]}};
      expect(bytes({event: "view", id: 1, payload: frame})).toBeLessThan(IPC_LIMIT);
    }
    // Typing a long comment: the body travels once, in the submit.
    const long = "x".repeat(10_000);
    const submit = Core.actionOfJson({ type: "DraftSubmitted", body: long });
    core.dispatch(submit._0);
    await tick();
    const [, args] = invoke.mock.calls.at(-1) as unknown as [string, unknown];
    expect(bytes(args)).toBeLessThan(IPC_LIMIT);
    // Scrolling: a viewport action is tiny regardless of file size.
    const scroll = Core.actionOfJson({
      type: "Viewport",
      file: actionFixture("Viewport").file,
      first_row: 99_940,
      last_row: 99_999,
    });
    core.dispatch(scroll._0);
    await tick();
    const [, scrollArgs] = invoke.mock.calls.at(-1) as unknown as [string, unknown];
    expect(bytes(scrollArgs)).toBeLessThan(200);
  });
});

describe("CoreWasm", () => {
  it("serves the empty model and refuses actions loudly", () => {
    const errors: string[] = [];
    const core = CoreWasm.make((e: string) => errors.push(e));
    const seen: unknown[] = [];
    core.subscribe((m: unknown) => seen.push(m));
    expect(seen.length).toBe(1);
    core.dispatch(Core.actionOfJson({ type: "Connect" })._0);
    expect(errors.length).toBe(1);
  });
});

it("Tauri reconstructs bounded fragments atomically and reattaches once on a gap", async () => {
  const errors: string[] = [];
  const core = CoreTauri.make((error: string) => errors.push(error));
  await tick(); core.attach(); await tick();
  let model: any, notifications = 0;
  core.subscribe((next: unknown) => { model = next; notifications += 1; });
  const original = patchFixtures();
  const connection = original.find(patch => patch.type === "Connection");
  const long = 'λ😀"\\'.repeat(20000);
  connection.last_error = {type: "Internal", message: long};
  const text = JSON.stringify(original), points = Array.from(text), pieces: string[] = [];
  for (let i = 0; i < points.length; i += 3000) pieces.push(points.slice(i, i + 3000).join(""));
  const frames = pieces.map((json, index) => ({revision: 1, kind: "Snapshot", body: {type: "Fragment", json,
    position: index === 0 ? {type: "Start", bytes: new TextEncoder().encode(text).length}
      : {type: index === pieces.length - 1 ? "End" : "More", index}}}));
  for (const frame of frames.slice(0, -1)) {
    expect(bytes({event: "view", id: 1, payload: frame})).toBeLessThan(IPC_LIMIT);
    emit(frame);
  }
  expect(notifications).toBe(1);
  emit(frames.at(-1)); expect(notifications).toBe(2);
  expect(model.last_error.message).toBe(long);
  emit({revision: 3, kind: "Delta", body: {type: "Complete", patches: []}});
  emit({revision: 4, kind: "Delta", body: {type: "Complete", patches: []}});
  await tick();
  expect(errors).toHaveLength(1);
  expect(invoke.mock.calls.filter(([command]) => command === "attach")).toHaveLength(2);
  expect(notifications).toBe(2);
  emit({revision: 5, kind: "Snapshot", body: {type: "Complete", patches: []}});
  expect(notifications).toBe(3);
  expect(model.last_error).toBeUndefined();
});
