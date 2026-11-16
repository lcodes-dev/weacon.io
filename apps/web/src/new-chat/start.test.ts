import assert from "node:assert/strict";
import { test } from "node:test";

import type { MachineSummary, SharedMachine } from "../protocol.ts";
import { agentMissing, callable, machineLabel, missingText, offeredAgents, readiness, startProblem } from "./start.ts";

const machine = (id: string, more: Partial<MachineSummary> = {}): MachineSummary => ({
  id,
  name: `Box ${id}`,
  os: "macos",
  key: "ab".repeat(32),
  paired_at: 1,
  ...more,
});

test("a person calls an agent only where the server takes commands, on a connected machine of theirs whose core runs chats", () => {
  const online = machine("mc-1", { connected: true });
  const old = machine("mc-2", { connected: true, missing: ["chat.serve"] });
  const away = machine("mc-3");

  const own = (id: string) => ({ id, name: `Box ${id}`, own: true, shared: false });
  assert.deepEqual(readiness(true, [away, online, old]), { kind: "ready", machines: [own("mc-1")] });
  assert.deepEqual(readiness(false, [online]), { kind: "unserved" });
  assert.deepEqual(readiness(true, []), { kind: "unpaired" });
  assert.deepEqual(readiness(true, [away]), { kind: "offline" });
  assert.deepEqual(readiness(true, [away, old]), { kind: "outdated" });
  const other = machine("mc-4", { connected: true, missing: ["terminal.relay"] });
  assert.deepEqual(readiness(true, [other]), { kind: "ready", machines: [own("mc-4")] });
});

test("a machine shared with the workspace is called like one's own, marked as shared, while it is connected", () => {
  const shared = (id: string, connected: boolean): SharedMachine => ({ id, name: `Box ${id}`, owner: "m-1", connected });
  const mine = machine("mc-1", { connected: true });
  const away = machine("mc-2");
  const others = [shared("mc-1", true), shared("mc-2", true), shared("mc-8", true), shared("mc-9", false)];

  const picked = callable([mine, away], others);

  // One's own shared machine is listed once, and one's own that is away isn't offered as someone else's.
  assert.deepEqual(picked, [
    { id: "mc-1", name: "Box mc-1", own: true, shared: true },
    { id: "mc-8", name: "Box mc-8", own: false, shared: true },
  ]);
  assert.deepEqual(picked.map(machineLabel), ["Box mc-1 (shared)", "Box mc-8 (shared)"]);
  assert.equal(machineLabel({ id: "mc-3", name: "Laptop", own: true, shared: false }), "Laptop");
  // Someone with no machine of their own calls the shared one.
  assert.deepEqual(readiness(true, [], [shared("mc-8", true)]), { kind: "ready", machines: [{ id: "mc-8", name: "Box mc-8", own: false, shared: true }] });
  assert.deepEqual(readiness(true, [], [shared("mc-9", false)]), { kind: "unpaired" });
  assert.deepEqual(readiness(false, [], [shared("mc-8", true)]), { kind: "unserved" });
});

test("each thing missing to call an agent reads as its own line", () => {
  const lines = (["unserved", "unpaired", "offline", "outdated"] as const).map(missingText);
  assert.equal(new Set(lines).size, 4);
  assert.equal(missingText("unpaired"), "Pair a machine in Settings to call your own agent.");
  assert.equal(missingText("offline"), "Your machine is offline. Start Weacon on it to call your agent.");
});

test("a first prompt that didn't start its chat says why, and the right way on", () => {
  assert.equal(startProblem("accepted"), undefined);
  assert.equal(startProblem("offline"), "The machine is offline, so the chat wasn't started. Try again once it's back.");
  assert.equal(startProblem("refused"), "The chat couldn't be started.");
  assert.equal(startProblem("full"), "This workspace is full, so the chat wasn't started.");
  assert.equal(startProblem("unsupported"), "The machine's core can't do this, so the chat couldn't be started.");
  assert.equal(startProblem("unsure"), "No answer came. The chat may have started.");
  assert.equal(startProblem("too_long"), "It's too long to send. Shorten it and try again.");
});

test("the fields say what the machine picked still needs before its agent can be called", () => {
  const ready = { repos: ["api"], agent: true };
  assert.equal(agentMissing(ready), undefined);
  assert.equal(agentMissing({ ...ready, repos: undefined }), "Looking for this machine's repositories.");
  assert.equal(agentMissing({ ...ready, repos: [] }), "Add a repository on this machine to call your agent.");
  assert.equal(agentMissing({ ...ready, agent: false }), "No agent is signed in for this workspace on this machine.");
});

test("every agent is offered until the machine answers, then only those signed in there for the workspace", () => {
  assert.deepEqual(offeredAgents(undefined), ["claude", "codex"]);
  assert.deepEqual(offeredAgents(["codex"]), ["codex"]);
  assert.deepEqual(offeredAgents(["codex", "claude"]), ["claude", "codex"]);
  assert.deepEqual(offeredAgents([]), []);
});
