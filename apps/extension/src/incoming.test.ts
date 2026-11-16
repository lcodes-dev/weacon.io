import assert from "node:assert/strict";
import { test } from "node:test";

import { parse } from "./incoming.ts";
import type { ChatEntry, Envelope } from "./protocol.ts";

const ABOUT = { workspace: "w-1", chat: "c-1" } as const;
const ENTRIES: ChatEntry[] = [
  { seq: 1, event: { kind: "prompt", request: "r-1", text: "Hi", pick: { url: "http://localhost/", title: "Home", selector: "p" } } },
  { seq: 2, event: { kind: "outside_prompt", text: "Hi" } },
  { seq: 3, event: { kind: "receipt", receipt: { request: "r-1", state: "accepted" } } },
  { seq: 4, event: { kind: "run_status", status: { run: "run-1", state: "running" } } },
  { seq: 5, event: { kind: "run", update: { run: "run-1", event: { event: "text", text: "Done." } } } },
  { seq: 6, event: { kind: "run", update: { run: "run-1", event: { event: "tool_call", call: "t1", tool: "Bash", input: {} } } } },
  { seq: 7, event: { kind: "run", update: { run: "run-1", event: { event: "tool_done", call: "t1", failed: false } } } },
  { seq: 8, event: { kind: "run", update: { run: "run-1", event: { event: "permission_asked", run: "run-1", prompt: 1, tool: "Bash", input: null } } } },
  { seq: 9, event: { kind: "run", update: { run: "run-1", event: { event: "permission_withdrawn", run: "run-1", prompt: 1 } } } },
  { seq: 10, event: { kind: "run", update: { run: "run-1", event: { event: "permission_answered", answer: { run: "run-1", prompt: 1, allow: true } } } } },
  { seq: 11, event: { kind: "run", update: { run: "run-1", event: { event: "finished", outcome: { state: "completed" } } } } },
];

const GOOD: Envelope[] = [
  { id: 1, type: "welcome", app: { name: "weacon-core", version: "0.1.0", commit: "1a2b3c4" }, capabilities: ["chat.list"] },
  { id: 1, type: "welcome", app: { name: "weacon-core", version: "0.1.0", commit: "1a2b3c4" } },
  { id: 2, type: "error", code: "chat.held", retryable: false },
  { id: 2, type: "receipt", request: "p-1", state: "completed" },
  ...ENTRIES.map((entry): Envelope => ({ id: 0, type: "chat", ...ABOUT, entry })),
  { id: 3, type: "chat_answer", ...ABOUT, answer: { kind: "snapshot", seq: 11, runs: [{ run: "run-1", state: "idle" }], waiting: [8], events: ENTRIES } },
  { id: 3, type: "chat_answer", ...ABOUT, answer: { kind: "window", events: ENTRIES } },
  { id: 0, type: "chat_answer", ...ABOUT, answer: { kind: "events_lost" } },
  {
    id: 4,
    type: "chats_answer",
    answer: { kind: "list", chats: [{ ...ABOUT, title: "Fix it", agent: "claude", updated_at: 1, run: "idle" }, { ...ABOUT, title: "", agent: "codex", updated_at: 2 }] },
  },
];

test("each message the panel acts on is parsed as it arrives", () => {
  for (const message of GOOD) {
    assert.deepEqual(parse(structuredClone(message)), message, JSON.stringify(message));
  }
});

test("newer kinds of event pass, to keep their place, and newer answers don't", () => {
  const newer = { id: 0, type: "chat", ...ABOUT, entry: { seq: 1, event: { kind: "note" } } };
  const thinking = { id: 0, type: "chat", ...ABOUT, entry: { seq: 1, event: { kind: "run", update: { run: "r", event: { event: "thinking" } } } } };

  assert.deepEqual(parse(newer), newer);
  assert.deepEqual(parse(thinking), thinking);
  assert.equal(parse({ id: 3, type: "chat_answer", ...ABOUT, answer: { kind: "moved" } }), undefined);
  assert.equal(parse({ id: 3, type: "chats_answer", answer: { kind: "page" } }), undefined);
});

test("anything malformed, or that the panel doesn't act on, is dropped", () => {
  const bad: unknown[] = [
    null,
    "text",
    [],
    { type: "welcome" },
    { id: -1, type: "error", code: "x.y" },
    { id: 1.5, type: "error", code: "x.y" },
    { id: 1, type: "run", run: "r", event: { event: "text", text: "x" } },
    { id: 1, type: "welcome", app: { name: "x", version: "1" } },
    { id: 1, type: "welcome", app: { name: "x", version: "1", commit: "c" }, capabilities: [1] },
    { id: 1, type: "error" },
    { id: 1, type: "receipt", request: "r" },
    { id: 0, type: "chat", workspace: "w", entry: ENTRIES[0] },
    { id: 0, type: "chat", ...ABOUT, entry: { seq: 0, event: { kind: "outside_prompt", text: "x" } } },
    { id: 0, type: "chat", ...ABOUT, entry: { seq: 1, event: { kind: "prompt", request: "r", text: "x", pick: { url: "u" } } } },
    { id: 0, type: "chat", ...ABOUT, entry: { seq: 1, event: { kind: "prompt", text: "x" } } },
    { id: 0, type: "chat", ...ABOUT, entry: { seq: 1, event: { kind: "receipt", receipt: {} } } },
    { id: 0, type: "chat", ...ABOUT, entry: { seq: 1, event: { kind: "run_status", status: { run: "r" } } } },
    { id: 0, type: "chat", ...ABOUT, entry: { seq: 1, event: { kind: "run", update: { event: { event: "text", text: "x" } } } } },
    { id: 0, type: "chat", ...ABOUT, entry: { seq: 1, event: { kind: "run", update: { run: "r", event: { event: "text" } } } } },
    { id: 0, type: "chat", ...ABOUT, entry: { seq: 1, event: { kind: "run", update: { run: "r", event: { event: "tool_call", call: "c" } } } } },
    { id: 0, type: "chat", ...ABOUT, entry: { seq: 1, event: { kind: "run", update: { run: "r", event: { event: "tool_done", call: "c" } } } } },
    { id: 0, type: "chat", ...ABOUT, entry: { seq: 1, event: { kind: "run", update: { run: "r", event: { event: "permission_asked", run: "r", prompt: 1 } } } } },
    { id: 0, type: "chat", ...ABOUT, entry: { seq: 1, event: { kind: "run", update: { run: "r", event: { event: "permission_withdrawn", run: "r" } } } } },
    { id: 0, type: "chat", ...ABOUT, entry: { seq: 1, event: { kind: "run", update: { run: "r", event: { event: "permission_answered", answer: { run: "r", prompt: 1 } } } } } },
    { id: 0, type: "chat", ...ABOUT, entry: { seq: 1, event: { kind: "run", update: { run: "r", event: { event: "finished", outcome: {} } } } } },
    { id: 0, type: "chat", ...ABOUT, entry: { seq: 1, event: { kind: "run", update: { run: "r", event: { event: 3 } } } } },
    { id: 3, type: "chat_answer", ...ABOUT, answer: { kind: "window", events: [{ seq: 1 }] } },
    { id: 3, type: "chat_answer", ...ABOUT, answer: { kind: "snapshot", seq: 1, runs: [{ run: "r" }], waiting: [], events: [] } },
    { id: 3, type: "chat_answer", ...ABOUT, answer: { kind: "snapshot", seq: 1, runs: [], waiting: [0], events: [] } },
    { id: 3, type: "chat_answer", ...ABOUT, answer: { kind: "snapshot", seq: -1, runs: [], waiting: [], events: [] } },
    { id: 4, type: "chats_answer", answer: { kind: "list", chats: [{ ...ABOUT, title: "x", agent: "claude", updated_at: "1" }] } },
    { id: 4, type: "chats_answer", answer: { kind: "list", chats: [{ ...ABOUT, title: "x", agent: "claude", updated_at: 1, run: 2 }] } },
    { id: 4, type: "chats_answer", answer: { kind: "list", chats: [{ ...ABOUT, agent: "claude", updated_at: 1 }] } },
  ];
  for (const message of bad) {
    assert.equal(parse(message), undefined, JSON.stringify(message));
  }
});
