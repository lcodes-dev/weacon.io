import assert from "node:assert/strict";
import { test } from "node:test";

import type { Incoming } from "./incoming.ts";
import type { ChatEntry, Envelope } from "./protocol.ts";
import { type Step, type Sync, hear, open } from "./sync.ts";

const REF = { workspace: "w-1", chat: "c-1" };
const entry = (seq: number): ChatEntry => ({ seq, event: { kind: "outside_prompt", text: `${seq}` } });
const asked = (seq: number): ChatEntry => ({ seq, event: { kind: "run", update: { run: "r", event: { event: "permission_asked", run: "r", prompt: seq, tool: "Bash", input: null } } } });

function ids(from = 10) {
  let at = from;
  return () => at++;
}

function snapshot(id: number, seq: number, events: ChatEntry[], waiting: number[] = []): Incoming {
  return { id, type: "chat_answer", ...REF, answer: { kind: "snapshot", seq, runs: [], waiting, events } };
}

const pushed = (seq: number, ref = REF): Incoming => ({ id: 0, type: "chat", ...ref, entry: entry(seq) });
const lost: Incoming = { id: 0, type: "chat_answer", ...REF, answer: { kind: "events_lost" } };
const refused = (id: number): Incoming => ({ id, type: "error", code: "chat.log_unreadable", retryable: false });

// The sync after `messages`, from an open chat, and every query sent on the way.
function after(messages: Incoming[], id = ids()): { sync: Sync; sent: Envelope[] } {
  let step: Step = open(REF, undefined, id);
  const sent = [...step.send];
  for (const message of messages) {
    step = hear(step.sync, message, id);
    sent.push(...step.send);
  }
  return { sync: step.sync, sent };
}

test("a snapshot, then a subscription after it, then each event pushed", () => {
  const { sync, sent } = after([snapshot(10, 2, [entry(1), entry(2)]), pushed(3), pushed(3, { workspace: "w-1", chat: "c-2" })]);

  assert.deepEqual(sent, [
    { id: 10, type: "chat_query", ...REF, query: { kind: "snapshot" } },
    { id: 11, type: "chat_query", ...REF, query: { kind: "subscribe", after: 2 } },
  ]);
  assert.equal(sync.chat?.seq, 3);
  assert.deepEqual(sync.state, { kind: "live", subscription: 11, window: undefined });
});

test("a stale snapshot, or an event before one, changes nothing", () => {
  const { sync, sent } = after([pushed(1), snapshot(9, 2, [entry(2)])]);

  assert.equal(sync.chat, undefined);
  assert.equal(sent.length, 1);
});

test("missed events, or the core saying they were lost, ask for a snapshot again keeping the chat", () => {
  const gap = after([snapshot(10, 2, [entry(2)]), pushed(4)]);
  const told = after([snapshot(10, 2, [entry(2)]), lost]);
  const early = after([lost]);

  for (const { sync, sent } of [gap, told]) {
    assert.deepEqual(sent.at(-1), { id: 12, type: "chat_query", ...REF, query: { kind: "snapshot" } });
    assert.deepEqual(sync.state, { kind: "asking", snapshot: 12 });
    assert.equal(sync.chat?.seq, 2);
  }
  assert.equal(early.sent.length, 1);
});

test("a waiting prompt older than the snapshot's events is fetched with a window", () => {
  const window = (id: number, events: ChatEntry[]): Incoming => ({ id, type: "chat_answer", ...REF, answer: { kind: "window", events } });
  const asking = after([snapshot(10, 9, [entry(8), entry(9)], [3])]);
  const fetched = after([snapshot(10, 9, [entry(8), entry(9)], [3]), window(9, [asked(7)]), window(12, [asked(3), entry(4), entry(5), entry(6), entry(7)])]);
  const empty = after([snapshot(10, 9, [entry(8), entry(9)], [3]), window(12, [])]);

  assert.deepEqual(asking.sent.at(-1), { id: 12, type: "chat_query", ...REF, query: { kind: "window", before: 8, limit: 5 } });
  assert.equal(fetched.sync.chat?.first, 3);
  assert.deepEqual(fetched.sync.state, { kind: "live", subscription: 11, window: undefined });
  assert.equal(fetched.sent.length, 3);
  assert.deepEqual(empty.sync.state, { kind: "live", subscription: 11, window: undefined });
  assert.equal(empty.sync.chat?.first, 8);
});

test("a refused snapshot or subscription fails the chat; a refused window only ends the wait", () => {
  const snapshotRefused = after([refused(10)]);
  const subscriptionRefused = after([snapshot(10, 1, [entry(1)]), refused(11)]);
  const windowRefused = after([snapshot(10, 9, [entry(9)], [3]), refused(12)]);
  const other = after([snapshot(10, 1, [entry(1)]), refused(99)]);

  assert.deepEqual(snapshotRefused.sync.state, { kind: "failed", code: "chat.log_unreadable" });
  assert.deepEqual(subscriptionRefused.sync.state, { kind: "failed", code: "chat.log_unreadable" });
  assert.deepEqual(windowRefused.sync.state, { kind: "live", subscription: 11, window: undefined });
  assert.deepEqual(other.sync.state, { kind: "live", subscription: 11, window: undefined });
  assert.deepEqual(after([refused(99)]).sync.state, { kind: "asking", snapshot: 10 });
});

test("an answer for another chat, or one the panel doesn't wait for, is ignored", () => {
  const elsewhere: Incoming = { id: 10, type: "chat_answer", workspace: "w-1", chat: "c-2", answer: { kind: "events_lost" } };
  const receipt: Incoming = { id: 10, type: "receipt", request: "r-1", state: "completed" };
  const { sync, sent } = after([elsewhere, receipt]);

  assert.deepEqual(sync.state, { kind: "asking", snapshot: 10 });
  assert.equal(sent.length, 1);
});
