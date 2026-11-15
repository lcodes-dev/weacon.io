import assert from "node:assert/strict";
import { test } from "node:test";

import type { MachineSummary } from "../protocol.ts";
import { Starts, missingText, readiness, startBlocked, startProblem } from "./start.ts";

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
  const other = machine("mc-4", { connected: true, missing: ["terminal.relay"] });
  assert.deepEqual(readiness(true, [other]), { kind: "ready", machines: [other] });
});

test("each thing missing reads as its own line, and nothing is missing once a chat can start", () => {
  const lines = (["unserved", "unpaired", "offline", "outdated"] as const).map(missingText);
  assert.equal(new Set(lines).size, 4);
  assert.equal(missingText("unpaired"), "Pair a machine in Settings to start a chat.");
  assert.equal(missingText("offline"), "Your machine is offline. Start Weacon on it to start a chat.");
});

test("a retry of a first prompt that may have run goes to the same chat with the same choice", () => {
  const starts = new Starts();
  let made = 0;
  const fresh = () => `c-${++made}`;
  const choice = { machine: "mc-1", repo: "api", agent: "codex" as const };
  const first = starts.begin("r-1", "w-1", "Fix the build.", choice, fresh);

  assert.deepEqual(first, {
    chatRef: { workspace: "w-1", chat: "c-1" },
    action: { kind: "prompt", text: "Fix the build.", machine: "mc-1", repo: "api", agent: "codex" },
  });
  assert.equal(starts.end("r-1", "lost"), undefined);
  assert.deepEqual(starts.begin("r-1", "w-1", "Fix the build.", { ...choice, repo: "web" }, fresh), first);
  assert.equal(starts.end("r-1", "unsure"), undefined);
  // A late receipt that accepts it opens the chat.
  assert.equal(starts.end("r-1", "accepted"), "c-1");
  assert.equal(starts.end("r-1", "accepted"), undefined);
});

test("a refused first prompt is forgotten, so the next send makes a fresh chat", () => {
  const starts = new Starts();
  let made = 0;
  const fresh = () => `c-${++made}`;
  const choice = { machine: "mc-1", repo: "api", agent: "claude" as const };
  starts.begin("r-1", "w-1", "Go.", choice, fresh);
  assert.equal(starts.end("r-1", "offline"), undefined);
  assert.equal(starts.begin("r-1", "w-1", "Go.", choice, fresh).chatRef.chat, "c-2");
  assert.equal(starts.end("r-1", "too_long"), undefined);
  assert.equal(starts.begin("r-1", "w-1", "Go.", choice, fresh).chatRef.chat, "c-3");
  assert.equal(starts.end("r-2", "accepted"), undefined);
});

test("a first prompt that didn't start its chat says why, and the right way on", () => {
  assert.equal(startProblem("accepted"), undefined);
  assert.equal(startProblem("offline"), "The machine is offline, so the chat wasn't started. Try again once it's back.");
  assert.equal(startProblem("refused"), "The chat couldn't be started.");
  assert.equal(startProblem("full"), "This workspace is full, so the chat wasn't started.");
  assert.equal(startProblem("unsupported"), "The machine's core can't do this, so the chat couldn't be started.");
  assert.equal(startProblem("unsure"), "The server didn't answer. The chat may have started.");
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
