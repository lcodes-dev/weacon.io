import assert from "node:assert/strict";
import { test } from "node:test";

import { type Answers, answerProblem, askKey, askText, sending, settle, stopProblem, wantsShown } from "./asks.ts";

const key = askKey("r-1", 2);
const mine = (allow: boolean) => ({ run: "r-1", prompt: 2, allow });
const none: Answers = new Map();

test("an answer on its way is kept under its request until it settles", () => {
  const first = sending(none, mine(true), "a-1");
  const changed = sending(settle(first, key, "a-1", "lost"), mine(false), "a-2");

  assert.deepEqual(first.get(key), { run: "r-1", prompt: 2, allow: true, request: "a-1", settled: undefined });
  assert.deepEqual(settle(first, key, "a-1", "lost").get(key)?.settled, "lost");
  assert.deepEqual(changed.get(key), { run: "r-1", prompt: 2, allow: false, request: "a-2", settled: undefined });
  assert.equal(none.size, 0);
});

test("a taken answer is forgotten, and a stale settling changes nothing", () => {
  const first = sending(none, mine(true), "a-1");

  assert.equal(settle(first, key, "a-1", "accepted").size, 0);
  assert.equal(settle(first, key, "a-9", "refused"), first);
  assert.equal(settle(first, askKey("r-1", 3), "a-1", "refused"), first);
  assert.equal(first.size, 1);
});

test("a prompt says why its answer may not have been taken", () => {
  const first = sending(none, mine(true), "a-1");

  assert.equal(answerProblem(undefined), undefined);
  assert.equal(answerProblem(first.get(key)), undefined);
  assert.equal(answerProblem(settle(first, key, "a-1", "refused").get(key)), "The answer wasn't taken.");
  assert.equal(
    answerProblem(settle(first, key, "a-1", "offline").get(key)),
    "The machine is offline, so the answer wasn't sent. Try again once it's back.",
  );
  assert.equal(answerProblem(settle(first, key, "a-1", "detached").get(key)), "Your agent's machine no longer has this chat, so it takes no answers.");
  assert.equal(answerProblem(settle(first, key, "a-1", "unsure").get(key)), "No answer came. The answer may not have been sent.");
});

test("Stop says why it may not have worked", () => {
  assert.equal(stopProblem("accepted"), undefined);
  assert.equal(stopProblem("refused"), "The agent wasn't stopped.");
  assert.equal(stopProblem("offline"), "The machine is offline, so the agent wasn't stopped. Try again once it's back.");
  assert.equal(stopProblem("detached"), "Your agent's machine no longer has this chat, so there's no agent to stop.");
  assert.equal(stopProblem("lost"), "The connection closed. The agent may not have stopped.");
  assert.equal(stopProblem("unsure"), "No answer came. The agent may not have stopped.");
});

test("a prompt names its tool, and says a tool when the name wasn't kept", () => {
  assert.equal(askText({ state: "open", tool: "Bash" }), "Asks to use Bash");
  assert.equal(askText({ state: "closed", tool: "Bash" }), "Asked to use Bash");
  assert.equal(askText({ state: "allowed", tool: "Bash" }), "Allowed Bash");
  assert.equal(askText({ state: "denied", tool: "Bash" }), "Denied Bash");
  assert.equal(askText({ state: "open", tool: "" }), "Asks to use a tool");
  assert.equal(askText({ state: "closed", tool: "" }), "Asked to use a tool");
  assert.equal(askText({ state: "allowed", tool: "" }), "Allowed");
  assert.equal(askText({ state: "denied", tool: "" }), "Denied");
});

test("a waiting prompt shows what its tool wants and says when it was cut, and an answered one shows neither", () => {
  assert.deepEqual(wantsShown({ state: "open", wants: { text: "cargo test" } }), { text: "cargo test", more: undefined });
  assert.deepEqual(wantsShown({ state: "open", wants: { text: "cargo", cut: true } }), { text: "cargo", more: "The rest is too long to show." });
  assert.equal(wantsShown({ state: "open", wants: undefined }), undefined);
  for (const state of ["closed", "allowed", "denied"] as const) {
    assert.equal(wantsShown({ state, wants: { text: "cargo test", cut: true } }), undefined);
  }
});
