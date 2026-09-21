// CoreWs adapter with a mocked WebSocket: attach-first on open, commands
// queue until the socket opens, patch frames apply to the store, and a
// close reconnects with a fresh attach.

import { readFileSync } from "node:fs";
import { join } from "node:path";
import { beforeEach, describe, expect, it, vi } from "vitest";

class FakeWebSocket {
  static instances: FakeWebSocket[] = [];
  url: string;
  sent: string[] = [];
  onopen: (() => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  onmessage: ((ev: { data: string }) => void) | null = null;
  constructor(url: string) {
    this.url = url;
    FakeWebSocket.instances.push(this);
  }
  send(text: string) {
    this.sent.push(text);
  }
  open() {
    this.onopen?.();
  }
  message(data: unknown) {
    this.onmessage?.({ data: JSON.stringify(data) });
  }
  close() {
    this.onclose?.();
  }
}
(globalThis as any).WebSocket = FakeWebSocket;

const CoreWs = await import("../src/core/CoreWs.res.mjs");
const Core = await import("../src/core/Core.res.mjs");

const fixtures = join(__dirname, "..", "..", "fixtures", "client");
const actionFixture = (name: string) =>
  JSON.parse(readFileSync(join(fixtures, "Action", `${name}.json`), "utf8"));
const patchFixture = (name: string) =>
  JSON.parse(readFileSync(join(fixtures, "ViewPatch", `${name}.json`), "utf8"));

const tick = () => new Promise((r) => setTimeout(r, 0));

beforeEach(() => {
  FakeWebSocket.instances = [];
  vi.useRealTimers();
});

describe("CoreWs", () => {
  it("queues commands until open, then attaches first", async () => {
    const core = CoreWs.make("ws://test", undefined);
    const action = Core.actionOfJson(actionFixture("Viewport"));
    expect(action.TAG).toBe("Ok");
    core.dispatch(action._0);
    const ws = FakeWebSocket.instances[0];
    expect(ws.sent).toEqual([]);
    ws.open();
    expect(ws.sent.length).toBe(2);
    expect(JSON.parse(ws.sent[0])).toEqual({ cmd: "attach" });
    expect(JSON.parse(ws.sent[1])).toEqual({ cmd: "dispatch", action: actionFixture("Viewport") });
  });

  it("applies patch frames to the store", async () => {
    const core = CoreWs.make("ws://test", undefined);
    const ws = FakeWebSocket.instances[0];
    ws.open();
    let model: any;
    core.subscribe((m: any) => (model = m));
    ws.message([patchFixture("Connection")]);
    expect(model.connection.TAG ?? model.connection).toBeDefined();
    expect(JSON.stringify(model.connection)).not.toContain("Disconnected");
  });

  it("resets session state, reconnects, and ignores its stale socket", async () => {
    vi.useFakeTimers();
    const core = CoreWs.make("ws://test", () => {});
    const first = FakeWebSocket.instances[0];
    let model: any;
    core.subscribe((m: any) => (model = m));
    first.open();
    first.message([patchFixture("Connection"), patchFixture("Hints")]);
    expect(JSON.stringify(model)).toContain("CopyPath");
    first.close();
    expect(JSON.stringify(model)).toContain("Disconnected");
    expect(JSON.stringify(model)).not.toContain("CopyPath");
    core.attach();
    vi.advanceTimersByTime(1500);
    expect(FakeWebSocket.instances.length).toBe(2);
    const second = FakeWebSocket.instances[1];
    second.open();
    expect(JSON.parse(second.sent[0])).toEqual({ cmd: "attach" });
    expect(second.sent.length).toBe(2);
    second.message([patchFixture("Connection")]);
    const current = JSON.stringify(model);

    // Neither a late frame nor another close callback from the replaced
    // socket may mutate the new session or schedule a third connection.
    first.message([patchFixture("Hints")]);
    first.close();
    vi.advanceTimersByTime(1500);
    expect(JSON.stringify(model)).toBe(current);
    expect(FakeWebSocket.instances.length).toBe(2);
  });
});

const creationFixture = () => JSON.parse(readFileSync(join(fixtures, "ReviewCreation", "default.json"), "utf8"));
const creationPatch = (creation: unknown, context = { type: "Named", name: "review-box" }) => {
  const patch = patchFixture("ReviewList");
  patch.home.creating = creation;
  patch.open_review = null;
  patch.daemon_context = context;
  return patch;
};
const dispatchJson = (core: any, action: unknown) => {
  const decoded = Core.actionOfJson(action);
  expect(decoded.TAG).toBe("Ok");
  core.dispatch(decoded._0);
};

it("recovers a browser-host lost ACK using the stable ID and frozen latest draft", () => {
  vi.useFakeTimers();
  const core = CoreWs.make("ws://test", () => {});
  const first = FakeWebSocket.instances[0];
  first.open();
  const creation = creationFixture();
  first.message([creationPatch(creation), patchFixture("Connection")]);
  const draft = { ...creation.draft, title: "latest input before submit" };
  dispatchJson(core, { type: "UpdateCreationDraft", review_id: creation.review_id, draft });
  dispatchJson(core, { type: "RunCommand", command: "Submit" });
  // Neither edit nor submit ACK arrived. A late key event cannot alter the
  // frozen attempt that may already have committed in the daemon.
  dispatchJson(core, { type: "UpdateCreationDraft", review_id: creation.review_id, draft: { ...draft, title: "too late" } });
  first.message([creationPatch(creation)]);
  first.close();
  vi.advanceTimersByTime(1000);
  const second = FakeWebSocket.instances[1];
  second.open();
  second.message([creationPatch(null), patchFixture("Connection")]);
  const actions = second.sent.map(text => JSON.parse(text)).filter(message => message.cmd === "dispatch");
  expect(actions).toHaveLength(1);
  expect(actions[0].action).toMatchObject({ type: "RestoreReviewCreation", resume: "Submitted", creation: { review_id: creation.review_id, draft } });
  expect(actions.some(message => ["SubmitReviewCreation", "CreateReview", "RunCommand"].includes(message.action.type))).toBe(false);
  second.message([creationPatch(null)]);
  expect(second.sent.filter(text => text.includes("RestoreReviewCreation"))).toHaveLength(1);
  second.message([creationPatch({ ...creation, status: { type: "Succeeded" } })]);
  second.close();
  vi.advanceTimersByTime(1000);
  const third = FakeWebSocket.instances[2];
  third.open();
  third.message([creationPatch(null), patchFixture("Connection")]);
  expect(third.sent).toHaveLength(1);
});

it("never restores a tab-local draft into another daemon context", () => {
  vi.useFakeTimers();
  const errors: string[] = [];
  const core = CoreWs.make("ws://test", (message: string) => errors.push(message));
  const first = FakeWebSocket.instances[0];
  first.open();
  first.message([creationPatch(creationFixture()), patchFixture("Connection")]);
  first.close();
  vi.advanceTimersByTime(1000);
  const second = FakeWebSocket.instances[1];
  second.open();
  second.message([creationPatch(null, { type: "Named", name: "another-daemon" }), patchFixture("Connection")]);
  expect(second.sent).toHaveLength(1);
  expect(errors.some(message => message.includes("different daemon context"))).toBe(true);
});

it("comment submit commands do not freeze a retained hidden review-creation draft", async () => {
  vi.useFakeTimers();
  const core = CoreWs.make("ws://test", () => {});
  const first = FakeWebSocket.instances[0];
  first.open();
  const creation = creationFixture();
  const retained = creationPatch(creation);
  retained.open_review = patchFixture("ReviewList").open_review;
  const hints = patchFixture("Hints");
  hints.bindings = [{ command: "Submit", keys: "ctrl+enter", label: "submit" }];
  first.message([retained, hints, patchFixture("Connection")]);
  dispatchJson(core, { type: "RunCommand", command: "Submit" });
  const Keys = await import("../src/view/Keys.res.mjs");
  core.key(Keys.ofBrowser({ key: "Enter", ctrlKey: true, altKey: false, shiftKey: false, metaKey: false }));
  first.close();
  vi.advanceTimersByTime(1000);
  const second = FakeWebSocket.instances[1];
  second.open();
  second.message([creationPatch(null), patchFixture("Connection")]);
  const actions = second.sent.map(text => JSON.parse(text)).filter(message => message.cmd === "dispatch");
  expect(actions).toHaveLength(1);
  expect(actions[0].action).toMatchObject({ type: "RestoreReviewCreation", resume: "Editing", creation: { review_id: creation.review_id } });
});
