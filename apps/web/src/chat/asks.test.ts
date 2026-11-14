import assert from "node:assert/strict";
import { test } from "node:test";

import { type Answers, answerProblem, askKey, sending, settle, settleAnswer, stopProblem } from "./asks.ts";

const key = askKey("r-1", 2);
const mine = (allow: boolean) => ({ run: "r-1", prompt: 2, allow });
const none: Answers = new Map();

function fresh() {
  let next = 0;
  return () => `a-${++next}`;
}

test("an answer that may have been taken goes again under its request, unless it changed", () => {
  const ids = fresh();
  const first = sending(none, mine(true), ids).answers;
  const lost = settle(first, key, "a-1", "lost");
  const again = sending(lost, mine(true), ids).answers;
  const changed = sending(lost, mine(false), ids).answers;
  const refused = sending(settle(first, key, "a-1", "refused"), mine(true), ids).answers;

  assert.deepEqual(first.get(key), { run: "r-1", prompt: 2, allow: true, request: "a-1", settled: undefined });
  assert.equal(sending(lost, mine(true), ids).request, "a-1");
  assert.deepEqual(lost.get(key), { run: "r-1", prompt: 2, allow: true, request: "a-1", settled: "lost" });
  assert.equal(again.get(key)?.request, "a-1");
  assert.equal(changed.get(key)?.request, "a-2");
  assert.equal(refused.get(key)?.request, "a-3");
  assert.equal(first.size, 1);
  assert.equal(none.size, 0);
});

test("a taken answer is forgotten, and a stale settling changes nothing", () => {
  const ids = fresh();
  const first = sending(none, mine(true), ids).answers;

  assert.equal(settle(first, key, "a-1", "accepted").size, 0);
  assert.equal(settle(first, key, "a-9", "refused"), first);
  assert.equal(settle(first, askKey("r-1", 3), "a-1", "refused"), first);
  assert.equal(first.size, 1);
});

test("a prompt says why its answer may not have been taken", () => {
  const ids = fresh();
  const first = sending(none, mine(true), ids).answers;

  assert.equal(answerProblem(undefined), undefined);
  assert.equal(answerProblem(first.get(key)), undefined);
  assert.equal(answerProblem(settle(first, key, "a-1", "refused").get(key)), "The answer wasn't taken.");
  assert.equal(answerProblem(settle(first, key, "a-1", "unsure").get(key)), "The server didn't answer. The answer may not have been sent.");
});

test("an answer settles under the prompt it was sent for, on time or late, and only its own", () => {
  const ids = fresh();
  const first = sending(none, mine(true), ids);
  const sent = new Map([
    ["a-1", { run: "r-1", prompt: 2 }],
    ["a-2", { run: "r-1", prompt: 2 }],
  ]);
  const unsure = settleAnswer(first.answers, sent, "a-1", "unsure");
  const replaced = sending(unsure, mine(false), ids).answers;

  assert.equal(settleAnswer(unsure, sent, "a-1", "accepted").size, 0);
  assert.equal(answerProblem(settleAnswer(unsure, sent, "a-1", "refused").get(key)), "The answer wasn't taken.");
  assert.equal(settleAnswer(unsure, sent, "q-9", "accepted"), unsure);
  // The late word on the first answer leaves the one that replaced it.
  assert.equal(settleAnswer(replaced, sent, "a-1", "accepted"), replaced);
  assert.equal(settleAnswer(replaced, sent, "a-2", "accepted").size, 0);
});

test("Stop says why it may not have worked", () => {
  assert.equal(stopProblem("accepted"), undefined);
  assert.equal(stopProblem("refused"), "The agent wasn't stopped.");
  assert.equal(stopProblem("lost"), "The connection closed. The agent may not have stopped.");
  assert.equal(stopProblem("unsure"), "The server didn't answer. The agent may not have stopped.");
});
