import assert from "node:assert/strict";
import { test } from "node:test";

import type { ChatEntry } from "../protocol.ts";
import { fromSnapshot, older } from "./chat.ts";
import { landing } from "./landing.ts";

const said = (seq: number, text: string): ChatEntry => ({ seq, event: { kind: "prompt", request: `q-${seq}`, text } });
const chatOf = (events: ChatEntry[], seq: number) => fromSnapshot({ seq, runs: new Map(), waiting: [], events });

test("a chat opened at a message loads older events until it holds it, then shows its line", () => {
  const chat = chatOf([said(5, "Late"), said(6, "Later")], 6);

  assert.deepEqual(landing(chat, 2), { kind: "older" });
  const widened = older(chat, [said(2, "Early"), said(3, "Then"), said(4, "Next")]);
  assert.deepEqual(landing(widened, 2), { kind: "line", key: "p2" });
  assert.deepEqual(landing(widened, 6), { kind: "line", key: "p6" });
  assert.deepEqual(landing(widened, 1), { kind: "older" });
  const whole = older(widened, [{ seq: 1, event: { kind: "receipt", receipt: { request: "q-1", state: "accepted" } } }]);
  assert.deepEqual(landing(whole, 1), { kind: "none" });
});

test("nothing lands without a chat, a message, or one the log holds", () => {
  const chat = chatOf([said(1, "Hi")], 1);

  assert.deepEqual(landing(undefined, 1), { kind: "none" });
  assert.deepEqual(landing(chat, undefined), { kind: "none" });
  assert.deepEqual(landing(chat, 2), { kind: "none" });
  assert.deepEqual(landing(chatOf([said(3, "Hi")], 3), 1), { kind: "older" });
  assert.deepEqual(landing({ ...chatOf([], 0), first: 1 }, 0), { kind: "none" });
});
