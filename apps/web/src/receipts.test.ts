import assert from "node:assert/strict";
import { test } from "node:test";

import type { Incoming } from "./incoming.ts";
import { Receipts, settledText } from "./receipts.ts";

const receipt = (request: string, state: "accepted" | "completed" | "interrupted" | "unknown"): Incoming => ({
  id: 2,
  type: "receipt",
  request,
  state,
});

function setup(): { receipts: Receipts; deadlines: Array<() => void> } {
  const deadlines: Array<() => void> = [];
  return { receipts: new Receipts((run) => deadlines.push(run)), deadlines };
}

test("a command ends with its first receipt, matched by request", async () => {
  const { receipts: all } = setup();
  const first = all.wait("r-1", 2);
  const second = all.wait("r-2", 3);
  const third = all.wait("r-3", 4);
  const fourth = all.wait("r-4", 5);

  all.take(receipt("r-2", "interrupted"));
  all.take(receipt("r-1", "accepted"));
  all.take(receipt("r-1", "unknown"));
  all.take({ id: 2, type: "receipt", request: "r-3", state: "failed", code: "run.failed" });
  assert.equal(await first, "accepted");
  assert.equal(await second, "refused");
  assert.equal(await third, "refused");
  all.take(receipt("r-4", "unknown"));
  assert.equal(await fourth, "unsure");
});

test("a command that ends at once counts as accepted", async () => {
  const { receipts: all } = setup();
  const done = all.wait("r-1", 6);
  all.take(receipt("r-1", "completed"));
  assert.equal(await done, "accepted");
});

test("other messages settle nothing", async () => {
  const { receipts: all } = setup();
  const waiting = all.wait("r-1", 2);
  all.take({ id: 3, type: "error", code: "request.conflict", retryable: false });
  all.end();
  assert.equal(await waiting, "lost");
});

test("a command is lost when the connection ends, and unsure after the deadline", async () => {
  const { receipts: all, deadlines } = setup();
  const lost = all.wait("r-1", 7);
  const late = all.wait("r-2", 8);
  deadlines[1]?.();
  all.end();
  // A receipt after the end changes nothing.
  all.take(receipt("r-1", "accepted"));
  deadlines[0]?.();

  assert.equal(await lost, "lost");
  assert.equal(await late, "unsure");
});

test("an error in answer to a command's envelope refuses it", async () => {
  const { receipts: all } = setup();
  const refused = all.wait("r-1", 7);
  const other = all.wait("r-2", 8);

  all.take({ id: 9, type: "error", code: "request.conflict", retryable: false });
  all.take({ id: 7, type: "error", code: "request.conflict", retryable: false });
  all.end();
  assert.equal(await refused, "refused");
  assert.equal(await other, "lost");
});

test("a refusal because the machine is offline or the chat detached says so, and no answer from it is unsure", async () => {
  const { receipts: all } = setup();
  const offline = all.wait("r-1", 7);
  const detached = all.wait("r-2", 8);
  const silent = all.wait("r-3", 9);

  all.take({ id: 7, type: "error", code: "machine.offline", retryable: true });
  all.take({ id: 8, type: "error", code: "chat.detached", retryable: false });
  all.take({ id: 9, type: "error", code: "machine.no_answer", retryable: true });
  assert.equal(await offline, "offline");
  assert.equal(await detached, "detached");
  assert.equal(await silent, "unsure");
});

test("a state this build doesn't know is unsure, whatever its name", async () => {
  const { receipts: all } = setup();
  const odd = all.wait("r-1", 2);
  all.take({ id: 2, type: "receipt", request: "r-1", state: "constructor" } as unknown as Incoming);
  assert.equal(await odd, "unsure");
});

test("how a command ended is said once for every subject", () => {
  const subject = { subject: "the answer", refused: "wasn't taken", unsent: "wasn't sent", unsure: "may not have been sent", gone: "it takes no answers" };
  const said = (["accepted", "refused", "offline", "detached", "working", "lost", "unsure"] as const).map((settled) => settledText(settled, subject));

  assert.deepEqual(said, [
    undefined,
    "The answer wasn't taken.",
    "The machine is offline, so the answer wasn't sent. Try again once it's back.",
    "Your machine no longer has this chat, so it takes no answers.",
    "Another person's agent is working in this chat, so the answer wasn't sent. Try again once it's done.",
    "The connection closed. The answer may not have been sent.",
    "No answer came. The answer may not have been sent.",
  ]);
});

test("a full workspace and a core that can't do a command each end it as their own refusal, as an error or a failed receipt", async () => {
  const { receipts: all } = setup();
  const full = all.wait("r-1", 2);
  const store = all.wait("r-2", 3);
  const unsupported = all.wait("r-3", 4);
  const failed = all.wait("r-4", 5);
  const other = all.wait("r-5", 6);
  const working = all.wait("r-6", 7);

  all.take({ id: 7, type: "error", code: "chat.agent_working", retryable: false });
  all.take({ id: 2, type: "error", code: "chat.quota_full", retryable: false });
  all.take({ id: 3, type: "error", code: "store.full", retryable: false });
  all.take({ id: 4, type: "error", code: "message.unsupported", retryable: false });
  all.take({ id: 9, type: "receipt", request: "r-4", state: "failed", code: "message.unsupported" });
  all.take({ id: 9, type: "receipt", request: "r-5", state: "failed", code: "chat.agent_conflict" });
  assert.deepEqual(await Promise.all([full, store, unsupported, failed, other]), ["full", "full", "unsupported", "unsupported", "refused"]);
  // Another person's agent on a turn refuses a call, which can go again once it is done.
  assert.equal(await working, "working");
  const about = { subject: "the message", refused: "wasn't sent", unsent: "wasn't sent", unsure: "may not have been sent", gone: "it takes none" };
  assert.equal(settledText("full", about), "This workspace is full, so the message wasn't sent.");
  assert.equal(settledText("unsupported", about), "The machine's core can't do this, so the message wasn't sent.");
});
