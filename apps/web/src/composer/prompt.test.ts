import assert from "node:assert/strict";
import { test } from "node:test";

import type { Envelope } from "../protocol.ts";
import { Receipts } from "../receipts.ts";
import { chatCommand } from "../chat/commands.ts";
import { after, blocked, deliverPrompt } from "./prompt.ts";

test("the box sends only to a chat, on a live connection to a server that takes messages", () => {
  const chatRef = { workspace: "w-1", chat: "c-1" };
  const server = { name: "weacon-server", version: "0.0.0", commit: "0000000" };
  const live = (capabilities: string[]) => ({ state: "live" as const, server, capabilities });

  assert.equal(blocked(live(["chat.send"]), chatRef), undefined);
  assert.equal(blocked(live(["chat.send"]), undefined), "Pick a chat to send to.");
  assert.equal(blocked(live(["run.stream"]), chatRef), "This server can't take messages yet.");
  assert.equal(blocked({ state: "connecting" }, chatRef), "Messages send once the server is connected.");
  assert.equal(blocked({ state: "ended", code: "connection.lost" }, chatRef), "Messages send once the server is connected.");
});

test("a prompt is sent and waits for its receipt, unless it is over the server's limit", async () => {
  const chatRef = { workspace: "w-1", chat: "c-1" };
  const receipts = new Receipts(() => {});
  const sent: Envelope[] = [];
  // With empty text the envelope takes 114 bytes, so 16,270 more fill the 16 KiB limit; あ takes 3.
  const fits = chatCommand(2, chatRef, "r-1", { kind: "prompt", text: `${"あ".repeat(5_423)}a` });
  const over = chatCommand(2, chatRef, "r-2", { kind: "prompt", text: `${"あ".repeat(5_423)}ab` });

  const waiting = deliverPrompt(fits, receipts, (envelope) => sent.push(envelope));
  assert.equal(await deliverPrompt(over, receipts, (envelope) => sent.push(envelope)), "too_long");
  receipts.take({ id: 2, type: "receipt", request: "r-1", state: "accepted" });

  assert.equal(await waiting, "accepted");
  assert.deepEqual(sent, [fits]);
});

test("an accepted prompt empties the box; any other keeps it and says why", () => {
  const retry = { text: "hi", request: "r-1" };
  assert.deepEqual(after("accepted", "hi", "r-1"), { text: "", problem: undefined, retry: undefined });
  assert.deepEqual(after("lost", "hi", "r-1"), {
    text: "hi",
    problem: "The connection closed. The message may not have been sent.",
    retry,
  });
  assert.deepEqual(after("unsure", "hi", "r-1"), {
    text: "hi",
    problem: "The server didn't answer. The message may not have been sent.",
    retry,
  });
  assert.deepEqual(after("refused", "hi", "r-1"), { text: "hi", problem: "The message wasn't sent.", retry: undefined });
  assert.deepEqual(after("too_long", "hi", "r-1"), {
    text: "hi",
    problem: "It's too long to send. Shorten it and try again.",
    retry: undefined,
  });
});

