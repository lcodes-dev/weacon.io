import assert from "node:assert/strict";
import { test } from "node:test";

import type { ChatMessage } from "../incoming.ts";
import { heard } from "./heard.ts";

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
  assert.deepEqual(heard(answer({ kind: "events_lost" }), ref), { kind: "lost" });
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
