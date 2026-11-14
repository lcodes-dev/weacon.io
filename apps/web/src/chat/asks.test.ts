import assert from "node:assert/strict";
import { test } from "node:test";

import { type Answers, answerProblem, askKey, sending, settle } from "./asks.ts";
import { answer, interrupt } from "./commands.ts";

const key = askKey("r-1", 2);
const none: Answers = new Map();

function fresh() {
  let next = 0;
  return () => `a-${++next}`;
}

test("an answer that may have been taken goes again under its request, unless it changed", () => {
  const ids = fresh();
  const first = sending(none, key, true, ids);
  const lost = settle(first, key, "a-1", "lost");
  const again = sending(lost, key, true, ids);
  const changed = sending(lost, key, false, ids);
  const refused = sending(settle(first, key, "a-1", "refused"), key, true, ids);

  assert.deepEqual(first.get(key), { allow: true, request: "a-1", settled: undefined });
  assert.deepEqual(lost.get(key), { allow: true, request: "a-1", settled: "lost" });
  assert.equal(again.get(key)?.request, "a-1");
  assert.equal(changed.get(key)?.request, "a-2");
  assert.equal(refused.get(key)?.request, "a-3");
  assert.equal(first.size, 1);
  assert.equal(none.size, 0);
});

test("a taken answer is forgotten, and a stale settling changes nothing", () => {
  const ids = fresh();
  const first = sending(none, key, true, ids);

  assert.equal(settle(first, key, "a-1", "accepted").size, 0);
  assert.equal(settle(first, key, "a-9", "refused"), first);
  assert.equal(settle(first, askKey("r-1", 3), "a-1", "refused"), first);
  assert.equal(first.size, 1);
});

test("a prompt says why its answer may not have been taken", () => {
  const ids = fresh();
  const first = sending(none, key, true, ids);

  assert.equal(answerProblem(undefined), undefined);
  assert.equal(answerProblem(first.get(key)), undefined);
  assert.equal(answerProblem(settle(first, key, "a-1", "refused").get(key)), "The answer wasn't taken");
  assert.equal(answerProblem(settle(first, key, "a-1", "unsure").get(key)), "The server didn't answer; the answer may not have been sent");
});

test("answers and interrupts are commands to the chat", () => {
  const place = { workspace: "w-1", chat: "c-1" };

  assert.deepEqual(answer(4, place, "a-1", "r-1", 2, false), {
    id: 4,
    type: "chat_command",
    workspace: "w-1",
    chat: "c-1",
    request: "a-1",
    action: { kind: "permission", answer: { run: "r-1", prompt: 2, allow: false } },
  });
  assert.deepEqual(interrupt(5, place, "s-1"), {
    id: 5,
    type: "chat_command",
    workspace: "w-1",
    chat: "c-1",
    request: "s-1",
    action: { kind: "interrupt" },
  });
});
