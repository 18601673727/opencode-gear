import { test } from "node:test";
import assert from "node:assert/strict";
import { createScenarioFixture } from "./scenarios";
import { createSnapshotEnvelopeFromFixture } from "./runtime-snapshot";
import { createRuntimeStore } from "./runtime-store";
import { RuntimeEnvelopeFactory } from "./runtime-envelope";
import type { RuntimeState } from "./reconciler";

function makeStore() {
  const fixture = createScenarioFixture("normal-chat");
  const seed = createSnapshotEnvelopeFromFixture(fixture, { streamId: "stream:store", generation: 1, sequence: 5 });
  return {
    store: createRuntimeStore(seed),
    seed,
    fixture,
    factory: new RuntimeEnvelopeFactory("stream:store", 1, { startSequence: 6 }),
  };
}

test("getState and getSnapshot keep stable identity between commits", () => {
  const { store } = makeStore();
  assert.equal(store.getState(), store.getState());
  assert.equal(store.getSnapshot(), store.getState().snapshot);
  assert.equal(store.getSnapshot(), store.getSnapshot());
});

test("state is committed before listeners are notified", () => {
  const { store, factory } = makeStore();
  let observedState: RuntimeState | null = null;
  const sequences: number[] = [];
  const unsubscribe = store.subscribe(() => {
    observedState = store.getState();
    sequences.push(store.getSync().cursor?.sequence ?? -1);
  });

  const mission = store.getSnapshot().missionsBySession["design-pwa-shell"]!;
  const envelope = factory.envelope(
    "mission.updated",
    { mission: { ...mission, current: "Committed first" } },
    { projectId: "zhuju", sessionId: "design-pwa-shell" },
  );
  store.applyEnvelope(envelope);

  assert.deepEqual(sequences, [6]);
  assert.equal(observedState!.snapshot.missionsBySession["design-pwa-shell"]?.current, "Committed first");

  unsubscribe();
  store.applyEnvelope(factory.envelope("runtime.status-changed", { status: { state: "failed" } }));
  assert.deepEqual(sequences, [6]);
});

test("applyMany commits once and a duplicate event never notifies", () => {
  const { store, factory } = makeStore();
  let notifications = 0;
  store.subscribe(() => { notifications += 1; });

  const first = factory.envelope("runtime.status-changed", { status: { state: "connecting" } });
  const second = factory.envelope("runtime.status-changed", { status: { state: "disconnected" } });
  store.applyMany([first, second]);
  assert.equal(notifications, 1);
  assert.equal(store.getSync().cursor?.sequence, 7);

  const before = store.getState();
  store.applyEnvelope(first); // duplicate event identity
  store.applyEnvelope(second); // duplicate event identity
  assert.equal(store.getState(), before);
  assert.equal(notifications, 1);
});

test("resetSnapshot installs a fresh baseline and clears diagnostics", () => {
  const { store, factory, fixture } = makeStore();
  const rejected = factory.envelope(
    "mission.launch-updated",
    {
      result: {
        outcome: "failed",
        commandId: "cmd-reset",
        draftId: "draft-reset",
        projectId: "zhuju",
        sessionId: "design-pwa-shell",
        message: "runtime down",
        duplicate: false,
      },
    },
    { projectId: "zhuju", sessionId: "design-pwa-shell" },
  );
  store.applyEnvelope(rejected);
  assert.ok(store.getDiagnostics().length > 0);

  const reset = createSnapshotEnvelopeFromFixture(fixture, { streamId: "stream:store", generation: 2, sequence: 0 });
  store.resetSnapshot(reset);
  assert.equal(store.getSync().generation, 2);
  assert.deepEqual(store.getDiagnostics(), []);
  assert.equal(store.getSync().cursor?.sequence, 0);
});

test("loading and reconnecting are observable degraded states", () => {
  const { store } = makeStore();
  store.markLoading();
  assert.equal(store.getSync().status, "loading-snapshot");
  store.markReconnecting();
  assert.equal(store.getSync().status, "reconnecting");
  assert.equal(store.getSync().resyncRequired, true);
});

test("unknown transport payloads are rejected before they can mutate canonical state", () => {
  const { store } = makeStore();
  const before = store.getSnapshot();
  store.applyUnknown({
    protocolVersion: 1,
    eventVersion: 1,
    streamId: "stream:store",
    generation: 1,
    sequence: 6,
    eventId: "bad:6",
    projectId: "zhuju",
    occurredAt: "not-a-date",
    type: "mission.updated",
    payload: { mission: null },
  });
  assert.equal(store.getSnapshot(), before);
  assert.equal(store.getSync().status, "error");
  assert.ok(store.getDiagnostics().some((diagnostic) => diagnostic.code === "schema-invalid"));
});

test("malformed snapshots are rejected without replacing the current baseline", () => {
  const { store } = makeStore();
  const before = store.getSnapshot();
  store.installUnknownSnapshot({ protocolVersion: 1, streamId: "stream:store", generation: 2 });
  assert.equal(store.getSnapshot(), before);
  assert.equal(store.getSync().status, "error");
  assert.ok(store.getDiagnostics().some((diagnostic) => diagnostic.code === "schema-invalid"));
});
