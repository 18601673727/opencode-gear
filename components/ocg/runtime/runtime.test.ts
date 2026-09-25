import { test } from "node:test";
import assert from "node:assert/strict";
import { MockOcgRuntimeClient } from "./mock-client";
import { resolveScenario } from "./scenarios";

test("scenario resolution falls back to normal chat", () => {
  assert.equal(resolveScenario("tool-heavy"), "tool-heavy");
  assert.equal(resolveScenario("not-a-scenario"), "normal-chat");
  assert.equal(resolveScenario(undefined), "normal-chat");
});

test("mock client exposes fixture sessions and missions", async () => {
  const client = new MockOcgRuntimeClient("budget-exhausted");
  const sessions = await client.listSessions();
  const mission = await client.getMission("design-pwa-shell");

  assert.ok(sessions.some((session) => session.id === "design-pwa-shell"));
  assert.equal(mission?.status, "budget-exhausted");
  assert.equal(mission?.budget.status, "exhausted");
});

test("mock client emits deterministic send events and supports unsubscribe", async () => {
  const client = new MockOcgRuntimeClient("normal-chat");
  const events: string[] = [];
  const unsubscribe = client.subscribe((event) => events.push(event.type));

  await client.sendMessage("design-pwa-shell", { content: "Test the local runtime" });
  await new Promise((resolve) => setTimeout(resolve, 350));

  assert.ok(events.includes("conversation.message-started"));
  assert.ok(events.includes("conversation.message-delta"));
  assert.ok(events.includes("conversation.message-completed"));
  assert.ok(events.includes("mission.updated"));
  const messages = await client.getMessages("design-pwa-shell");
  assert.equal(messages.at(-1)?.status, "completed");

  const eventCount = events.length;
  unsubscribe();
  await client.createSession({ workType: "coding" });
  assert.equal(events.length, eventCount);
});

test("mock client cancels an active local stream", async () => {
  const client = new MockOcgRuntimeClient("long-stream");
  await client.sendMessage("design-pwa-shell", { content: "Cancel this" });
  await client.cancel?.("design-pwa-shell");

  const messages = await client.getMessages("design-pwa-shell");
  assert.equal(messages.at(-1)?.status, "cancelled");
});

test("disconnected runtime does not pretend to send a message", async () => {
  const client = new MockOcgRuntimeClient("runtime-disconnected");
  const before = await client.getMessages("design-pwa-shell");
  const events: string[] = [];
  client.subscribe((event) => events.push(event.type));

  await client.sendMessage("design-pwa-shell", { content: "This stays local" });

  assert.deepEqual(await client.getMessages("design-pwa-shell"), before);
  assert.ok(events.includes("warning"));
});
