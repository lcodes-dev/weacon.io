import assert from "node:assert/strict";
import { test } from "node:test";

import type { Incoming } from "./incoming.ts";
import { Receipts } from "./receipts.ts";

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

test("waiting again for a request still waiting shares its end, and its deadline", async () => {
  const { receipts: all, deadlines } = setup();
  const first = all.wait("r-1", 2);
  assert.equal(all.wait("r-1", 3), first);
  assert.equal(deadlines.length, 1);
  all.take(receipt("r-1", "accepted"));
  assert.equal(await first, "accepted");
});

test("an earlier send's deadline doesn't end a later send of the same request", async () => {
  const { receipts: all, deadlines } = setup();
  all.take(receipt("r-1", "unknown"));
  const first = all.wait("r-1", 2);
  all.take(receipt("r-1", "unknown"));
  assert.equal(await first, "unsure");
  const again = all.wait("r-1", 3);
  deadlines[0]?.();
  all.take(receipt("r-1", "accepted"));
  assert.equal(await again, "accepted");
});

test("a state this build doesn't know is unsure, whatever its name", async () => {
  const { receipts: all } = setup();
  const odd = all.wait("r-1", 2);
  all.take({ id: 2, type: "receipt", request: "r-1", state: "constructor" } as unknown as Incoming);
  assert.equal(await odd, "unsure");
});
