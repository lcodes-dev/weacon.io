import assert from "node:assert/strict";
import { test } from "node:test";

import type { Envelope } from "../protocol.ts";
import { Receipts } from "../receipts.ts";
import { chatCommand } from "../chat/commands.ts";
import { type Sent, blocked, deliverPrompt, notingPlain, sentProblem } from "./prompt.ts";

test("the box sends only to a chat, on a live connection to a server that takes messages", () => {
  const chatRef = { workspace: "w-1", chat: "c-1" };

  assert.equal(blocked(true, chatRef, true), undefined);
  assert.equal(blocked(true, undefined, true), "Pick a chat to send to.");
  assert.equal(blocked(true, chatRef, false), "This server can't take messages yet.");
  assert.equal(blocked(false, chatRef, true), "Messages send once the server is connected.");
});

test("a prompt is sent and waits for its receipt, unless it is over the server's limit", async () => {
  const chatRef = { workspace: "w-1", chat: "c-1" };
  const receipts = new Receipts(() => {});
  const sent: Envelope[] = [];
  // With empty text the envelope takes 114 bytes, so 1,015,694 more fill the limit; あ takes 3.
  const fits = chatCommand(2, chatRef, "r-1", { kind: "prompt", text: `${"あ".repeat(338_564)}aa` });
  const over = chatCommand(2, chatRef, "r-2", { kind: "prompt", text: `${"あ".repeat(338_564)}aaa` });

  const waiting = deliverPrompt(fits, receipts, (envelope) => sent.push(envelope));
  assert.equal(await deliverPrompt(over, receipts, (envelope) => sent.push(envelope)), "too_long");
  receipts.take({ id: 2, type: "receipt", request: "r-1", state: "accepted" });

  assert.equal(await waiting, "accepted");
  assert.deepEqual(sent, [fits]);
});

test("a send says why it wasn't taken, or may not have been, and nothing once it was", () => {
  assert.equal(sentProblem("accepted"), undefined);
  assert.equal(sentProblem("lost"), "The connection closed. The message may not have been sent.");
  assert.equal(sentProblem("unsure"), "No answer came. The message may not have been sent.");
  assert.equal(sentProblem("refused"), "The message wasn't sent.");
  assert.equal(sentProblem("offline"), "The machine is offline, so the message wasn't sent. Try again once it's back.");
  assert.equal(sentProblem("detached"), "Your agent's machine no longer has this chat, so your agent can't be called here.");
  assert.equal(sentProblem("too_long"), "It's too long to send. Shorten it and try again.");
});

test("only a taken message that calls no agent is noted as plain", async () => {
  const noted: string[] = [];
  const ends = new Map<string, Sent>([["taken", "accepted"], ["kept", "refused"]]);
  const send = notingPlain(async (text) => ends.get(text) ?? "unsure", () => noted.push("plain"));
  assert.equal(await send("taken", "r1" as never, true), "accepted");
  assert.equal(await send("kept", "r2" as never, false), "refused");
  assert.deepEqual(noted, []);
  assert.equal(await send("taken", "r3" as never, false), "accepted");
  assert.deepEqual(noted, ["plain"]);
});
