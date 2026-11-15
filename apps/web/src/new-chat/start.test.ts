import assert from "node:assert/strict";
import { test } from "node:test";

import type { MachineSummary } from "../protocol.ts";
import { missingText, readiness, startBlocked, startProblem } from "./start.ts";

const machine = (id: string, more: Partial<MachineSummary> = {}): MachineSummary => ({
  id,
  name: `Box ${id}`,
  os: "macos",
  key: "ab".repeat(32),
  paired_at: 1,
  ...more,
});

test("a chat starts only where the server takes commands, on a connected machine whose core runs chats", () => {
  const online = machine("mc-1", { connected: true });
  const old = machine("mc-2", { connected: true, missing: ["chat.serve"] });
  const away = machine("mc-3");

  assert.deepEqual(readiness(true, [away, online, old]), { kind: "ready", machines: [online] });
  assert.deepEqual(readiness(false, [online]), { kind: "unserved" });
  assert.deepEqual(readiness(true, []), { kind: "unpaired" });
  assert.deepEqual(readiness(true, [away]), { kind: "offline" });
  assert.deepEqual(readiness(true, [away, old]), { kind: "outdated" });
  const silent = machine("mc-5", { connected: true, missing: ["chat.answers"] });
  assert.deepEqual(readiness(true, [silent]), { kind: "outdated" });
  const other = machine("mc-4", { connected: true, missing: ["terminal.relay"] });
  assert.deepEqual(readiness(true, [other]), { kind: "ready", machines: [other] });
});

test("each thing missing reads as its own line, and nothing is missing once a chat can start", () => {
  const lines = (["unserved", "unpaired", "offline", "outdated"] as const).map(missingText);
  assert.equal(new Set(lines).size, 4);
  assert.equal(missingText("unpaired"), "Pair a machine in Settings to start a chat.");
  assert.equal(missingText("offline"), "Your machine is offline. Start Weacon on it to start a chat.");
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

test("the box says what the machine picked still needs before a chat can start there", () => {
  const ready = { live: true, repos: ["api"], agent: "Codex", signIn: "signed_in" as const };
  assert.equal(startBlocked(ready), undefined);
  assert.equal(startBlocked({ ...ready, signIn: undefined }), undefined);
  assert.equal(startBlocked({ ...ready, live: false }), "Chats start once the server is connected.");
  assert.equal(startBlocked({ ...ready, repos: undefined }), "Pick a repository to start a chat.");
  assert.equal(startBlocked({ ...ready, repos: [] }), "Add a repository on this machine to start a chat.");
  assert.equal(startBlocked({ ...ready, signIn: "signed_out" }), "Codex isn't signed in on this machine.");
  assert.equal(startBlocked({ ...ready, signIn: "not_installed" }), "Codex isn't installed on this machine.");
});
