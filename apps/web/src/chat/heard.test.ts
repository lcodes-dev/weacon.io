import assert from "node:assert/strict";
import { test } from "node:test";

import type { ChatMessage } from "../incoming.ts";
import { heard, receiptsIn } from "./heard.ts";

const ref = { workspace: "w-1", chat: "c-1" };
const said = { seq: 1, event: { kind: "prompt" as const, request: "q-1", text: "Hi" } };
const update: ChatMessage = { id: 7, type: "chat", ...ref, entry: said };
const answer = (body: Extract<ChatMessage, { type: "chat_answer" }>["answer"]): ChatMessage => ({
  id: 7,
  type: "chat_answer",
  ...ref,
  answer: body,
});

test("this chat's events and answers are heard, under the envelope they answer", () => {
  const agent = { kind: "claude" as const, owner: { kind: "shared" as const } };
  const snapshot = answer({ kind: "snapshot", seq: 4, runs: [{ run: "r-1", state: "waiting", agent }, { run: "r-2", state: "idle" }], waiting: [3], events: [said] });

  assert.deepEqual(heard(update, ref), { kind: "entry", entry: said });
  assert.deepEqual(heard(answer({ kind: "events_lost" }), ref), { kind: "lost", id: 7 });
  assert.deepEqual(heard(answer({ kind: "window", events: [said] }), ref), { kind: "window", id: 7, events: [said] });
  assert.deepEqual(heard(snapshot, ref), {
    kind: "snapshot",
    id: 7,
    snapshot: {
      seq: 4,
      runs: new Map([
        ["r-1", { state: "waiting", agent }],
        ["r-2", { state: "idle", agent: undefined }],
      ]),
      waiting: [3],
      events: [said],
    },
  });
});

test("another chat's or workspace's messages aren't heard, and run updates always are", () => {
  const delta = { event: "text_delta" as const, text: "He" };

  assert.equal(heard({ ...update, chat: "c-2" }, ref), undefined);
  assert.equal(heard({ ...update, workspace: "w-2" }, ref), undefined);
  assert.equal(heard({ ...answer({ kind: "events_lost" }), chat: "c-2" }, ref), undefined);
  assert.deepEqual(heard({ id: 0, type: "run", run: "r-9", event: delta }, ref), { kind: "run", run: "r-9", event: delta });
  assert.deepEqual(heard({ id: 6, type: "error", code: "chat.unknown", retryable: false }, ref), { kind: "refused", id: 6 });
});

test("a committed receipt is taken as the receipt of the command it answers, live or in an answer", () => {
  const receipt = { kind: "receipt" as const, receipt: { request: "q-1", state: "accepted" as const } };
  const failed = { kind: "receipt" as const, receipt: { request: "q-2", state: "failed" as const, code: "run.failed" } };
  const accepted = { id: 7, type: "receipt", request: "q-1", state: "accepted" };
  const refused = { id: 7, type: "receipt", request: "q-2", state: "failed", code: "run.failed" };
  const events = [{ seq: 2, event: receipt }, said, { seq: 3, event: failed }];

  assert.deepEqual(receiptsIn({ ...update, entry: { seq: 2, event: receipt } }), [accepted]);
  assert.deepEqual(receiptsIn({ ...update, entry: { seq: 3, event: failed } }), [refused]);
  assert.deepEqual(receiptsIn(answer({ kind: "window", events })), [accepted, refused]);
  assert.deepEqual(receiptsIn(answer({ kind: "snapshot", seq: 3, runs: [], waiting: [], events })), [accepted, refused]);
  assert.deepEqual(receiptsIn(update), []);
  assert.deepEqual(receiptsIn(answer({ kind: "events_lost" })), []);
  assert.deepEqual(receiptsIn({ id: 6, type: "error", code: "chat.unknown", retryable: false }), []);
});
