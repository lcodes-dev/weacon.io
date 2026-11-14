import assert from "node:assert/strict";
import { test } from "node:test";

import type { Envelope } from "../protocol.ts";
import { Receipts } from "../receipts.ts";
import { after, blocked, deliver, prompt, requestFor } from "./prompt.ts";

test("a prompt is a chat command for the chat, under its request", () => {
  assert.deepEqual(prompt(3, { workspace: "w-1", chat: "c-1" }, "r-1", "日本語"), {
    id: 3,
    type: "chat_command",
    workspace: "w-1",
    chat: "c-1",
    request: "r-1",
    action: { kind: "prompt", text: "日本語" },
  });
});

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
  const fits = prompt(2, chatRef, "r-1", `${"あ".repeat(5_423)}a`);
  const over = prompt(2, chatRef, "r-2", `${"あ".repeat(5_423)}ab`);

  const waiting = deliver(fits, receipts, (envelope) => sent.push(envelope));
  assert.equal(await deliver(over, receipts, (envelope) => sent.push(envelope)), "too_long");
  receipts.take({ id: 2, type: "receipt", request: "r-1", state: "accepted" });

  assert.equal(await waiting, "accepted");
  assert.deepEqual(sent, [fits]);
});

test("a prompt that may have run is retried under its own request, and only while its text is the same", () => {
  const fresh = () => "r-new";
  assert.equal(requestFor(undefined, "hi", fresh), "r-new");
  assert.equal(requestFor({ text: "hi", request: "r-1" }, "hi", fresh), "r-1");
  assert.equal(requestFor({ text: "hi", request: "r-1" }, "hi!", fresh), "r-new");

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

test("a prompt the socket can't take is lost", async () => {
  const command = prompt(2, { workspace: "w-1", chat: "c-1" }, "r-1", "hi");
  const failing = () => {
    throw new Error("closed");
  };
  const receipts = new Receipts(() => {});
  assert.equal(await deliver(command, receipts, failing), "lost");

  const retry = prompt(3, { workspace: "w-1", chat: "c-1" }, "r-1", "hi");
  const waiting = deliver(retry, receipts, () => {});
  receipts.take({ id: 3, type: "error", code: "request.conflict", retryable: false });
  assert.equal(await waiting, "refused");
});

