import assert from "node:assert/strict";
import { test } from "node:test";

import { type Answers, answerProblem, answeredUnder, askKey, sending, settle, settleLate } from "./asks.ts";
import { chatCommand } from "../composer/prompt.ts";

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
  assert.equal(answerProblem(settle(first, key, "a-1", "refused").get(key)), "The answer wasn't taken");
  assert.equal(answerProblem(settle(first, key, "a-1", "unsure").get(key)), "The server didn't answer; the answer may not have been sent");
});

test("a chat command carries its chat, request and action", () => {
  const ref = { workspace: "w-1", chat: "c-1" };

  assert.deepEqual(chatCommand(4, ref, "a-1", { kind: "permission", answer: { run: "r-1", prompt: 2, allow: false } }), {
    id: 4,
    type: "chat_command",
    workspace: "w-1",
    chat: "c-1",
    request: "a-1",
    action: { kind: "permission", answer: { run: "r-1", prompt: 2, allow: false } },
  });
  assert.deepEqual(chatCommand(5, ref, "s-1", { kind: "interrupt" }), {
    id: 5,
    type: "chat_command",
    workspace: "w-1",
    chat: "c-1",
    request: "s-1",
    action: { kind: "interrupt" },
  });
});

test("a late receipt settles the answer sent under its request", () => {
  const ids = fresh();
  const unsure = settle(sending(none, mine(true), ids).answers, key, "a-1", "unsure");

  assert.equal(settleLate(unsure, "a-1", "accepted").size, 0);
  assert.equal(answerProblem(settleLate(unsure, "a-1", "refused").get(key)), "The answer wasn't taken");
  assert.equal(settleLate(unsure, "a-9", "accepted"), unsure);
  assert.equal(answeredUnder(unsure, "a-1")?.prompt, 2);
  assert.equal(answeredUnder(unsure, "a-9"), undefined);
});
