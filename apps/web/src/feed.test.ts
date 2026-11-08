import assert from "node:assert/strict";
import { test } from "node:test";

import { Feed } from "./feed.ts";
import type { Envelope, RunEvent } from "./protocol.ts";

const run = (id: string, text: string): Envelope => ({ id: 0, type: "run", run: id, event: { event: "text_delta", text } });

test("the first run's events arrive in order, one batch per scheduled call", () => {
  const scheduled: (() => void)[] = [];
  const batches: RunEvent[][] = [];
  const feed = new Feed(
    (flush) => scheduled.push(flush),
    (events) => batches.push(events),
  );
  feed.push(run("r-1", "a"));
  feed.push({ id: 0, type: "error", code: "store.full", retryable: false });
  feed.push(run("r-2", "other"));
  feed.push({ id: 0, type: "run", run: "r-1" } as unknown as Envelope);
  feed.push({ id: 0, type: "run", run: 7, event: { event: "text_delta", text: "x" } } as unknown as Envelope);
  feed.push(run("r-1", "b"));
  scheduled.shift()?.();
  feed.push(run("r-1", "c"));
  scheduled.shift()?.();

  assert.equal(scheduled.length, 0);
  assert.deepEqual(
    batches.map((events) => events.map((event) => (event.event === "text_delta" ? event.text : ""))),
    [["a", "b"], ["c"]],
  );
});
