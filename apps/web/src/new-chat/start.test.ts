import assert from "node:assert/strict";
import { test } from "node:test";

import type { MachineSettings, MachineSummary } from "../protocol.ts";
import { agentMissing, missingText, readiness, signedIn, startProblem } from "./start.ts";

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

  assert.deepEqual(readiness(true, [away, online, old]), { kind: "ready", machines: [online] });
  assert.deepEqual(readiness(false, [online]), { kind: "unserved" });
  assert.deepEqual(readiness(true, []), { kind: "unpaired" });
  assert.deepEqual(readiness(true, [away]), { kind: "offline" });
  assert.deepEqual(readiness(true, [away, old]), { kind: "outdated" });
  const other = machine("mc-4", { connected: true, missing: ["terminal.relay"] });
  assert.deepEqual(readiness(true, [other]), { kind: "ready", machines: [other] });
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
  const settings = (states: MachineSettings["sign_ins"]): MachineSettings => ({ sign_ins: states }) as unknown as MachineSettings;
  const mixed = settings([
    { agent: "claude", state: "signed_out" },
    { agent: "codex", state: "signed_in" },
  ]);

  assert.deepEqual(signedIn(undefined), ["claude", "codex"]);
  assert.deepEqual(signedIn(mixed), ["codex"]);
  assert.deepEqual(signedIn(settings([])), []);
});
