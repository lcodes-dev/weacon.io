import assert from "node:assert/strict";
import { test } from "node:test";

import { Receipts } from "../receipts.ts";
import { chatCommand, deliver } from "./commands.ts";

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

test("a command waits for its receipt, and one the socket can't take is lost", async () => {
  const ref = { workspace: "w-1", chat: "c-1" };
  const receipts = new Receipts(() => {});
  const failing = () => {
    throw new Error("closed");
  };
  const taken = deliver(chatCommand(2, ref, "s-1", { kind: "interrupt" }), receipts, () => {});
  receipts.take({ id: 2, type: "receipt", request: "s-1", state: "accepted" });

  assert.equal(await taken, "accepted");
  assert.equal(await deliver(chatCommand(3, ref, "s-2", { kind: "interrupt" }), receipts, failing), "lost");
  // Sent again under the same request, it waits afresh, under its own envelope.
  const retry = deliver(chatCommand(4, ref, "s-2", { kind: "interrupt" }), receipts, () => {});
  receipts.take({ id: 4, type: "error", code: "request.conflict", retryable: false });
  assert.equal(await retry, "refused");
});
