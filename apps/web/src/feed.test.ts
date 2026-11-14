import assert from "node:assert/strict";
import { test } from "node:test";

import { Feed, nextFrame } from "./feed.ts";
import type { RunMessage } from "./incoming.ts";
import type { RunEvent } from "./protocol.ts";

const run = (id: string, text: string): RunMessage => ({ id: 0, type: "run", run: id, event: { event: "text_delta", text } });

test("the first run's events arrive in order, one batch per scheduled call", () => {
  const scheduled: (() => void)[] = [];
  const batches: RunEvent[][] = [];
  const feed = new Feed(
    (flush) => scheduled.push(flush),
    (events) => batches.push(events),
  );
  feed.push(run("r-1", "a"));
  feed.push(run("r-2", "other"));
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

test("a batch scheduled twice goes out once, and a newer batch isn't held back", () => {
  const scheduled: (() => void)[] = [];
  const batches: RunEvent[][] = [];
  const feed = new Feed(
    (flush) => scheduled.push(flush, flush),
    (events) => batches.push(events),
  );
  feed.push(run("r-1", "a"));
  scheduled.shift()?.();
  feed.push(run("r-1", "b"));
  scheduled.shift()?.();
  scheduled.shift()?.();
  scheduled.shift()?.();

  assert.deepEqual(
    batches.map((events) => events.map((event) => (event.event === "text_delta" ? event.text : ""))),
    [["a"], ["b"]],
  );
});

test("a batch goes out on the next frame, or on the timer once the tab is hidden", () => {
  let hidden = false;
  const frames = new Map<number, () => void>();
  const timers: (() => void)[] = [];
  const schedule = nextFrame(
    () => hidden,
    (run) => frames.set(frames.size + 1, run).size,
    (frame) => frames.delete(frame),
    (run) => timers.push(run),
  );
  const sent: string[] = [];

  schedule(() => sent.push("shown"));
  timers.shift()?.();
  assert.equal(sent.length, 0);
  frames.get(1)?.();
  assert.deepEqual(sent, ["shown"]);

  schedule(() => sent.push("hidden"));
  hidden = true;
  timers.shift()?.();
  assert.deepEqual(sent, ["shown", "hidden"]);
  assert.deepEqual([...frames.keys()], [1]);
});
