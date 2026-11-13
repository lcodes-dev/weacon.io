import assert from "node:assert/strict";
import { test } from "node:test";

import type { RunEvent } from "./protocol.ts";
import { EMPTY, apply, isRunEvent } from "./transcript.ts";

const delta = (text: string): RunEvent => ({ event: "text_delta", text });
const whole = (text: string): RunEvent => ({ event: "text", text });
const call = (id: string): RunEvent => ({ event: "tool_call", call: id, tool: "Read", input: {} });
const done = (id: string, failed: boolean): RunEvent => ({ event: "tool_done", call: id, failed });

test("deltas stream into one row that the whole text then replaces", () => {
  const streamed = apply(EMPTY, [delta("He"), delta("llo")]);
  const ended = apply(streamed, [whole("Hello.")]);
  const next = apply(ended, [whole("Next.")]);

  const same = apply(apply(EMPTY, [delta("Hi"), delta(" there")]), [whole("Hi there")]);

  assert.deepEqual(streamed, { rows: [{ kind: "text", pieces: ["He", "llo"] }], writing: true });
  assert.deepEqual(ended, { rows: [{ kind: "text", pieces: ["Hello."] }], writing: false });
  assert.deepEqual(next.rows, [
    { kind: "text", pieces: ["Hello."] },
    { kind: "text", pieces: ["Next."] },
  ]);
  // Whole text that matches what streamed keeps the pieces, so a selection in them survives.
  assert.deepEqual(same, { rows: [{ kind: "text", pieces: ["Hi", " there"] }], writing: false });
});

test("tools get rows that change when they finish, and end the text", () => {
  const { rows, writing } = apply(EMPTY, [
    delta("Reading"),
    call("c-1"),
    call("c-2"),
    done("c-1", false),
    done("c-2", true),
    done("c-1", true),
    done("c-9", false),
    delta("Done"),
  ]);

  assert.deepEqual(rows, [
    { kind: "text", pieces: ["Reading"] },
    { kind: "tool", call: "c-1", tool: "Read", state: "done" },
    { kind: "tool", call: "c-2", tool: "Read", state: "failed" },
    { kind: "text", pieces: ["Done"] },
  ]);
  assert.equal(writing, true);
});

test("a finished turn ends the text and its running tools, and other events change nothing", () => {
  const before = apply(EMPTY, [call("c-1"), done("c-1", false), call("c-2"), delta("Bye")]);
  const after = apply(before, [
    { event: "started", session: "0b8f5c2e-7a1d-4e3b-9c6f-2d4a8e1b5c7f" },
    { event: "finished", outcome: { state: "completed" } },
    { event: "newer" } as unknown as RunEvent,
    delta("Hi"),
  ]);

  assert.deepEqual(after.rows, [
    { kind: "tool", call: "c-1", tool: "Read", state: "done" },
    { kind: "tool", call: "c-2", tool: "Read", state: "failed" },
    { kind: "text", pieces: ["Bye"] },
    { kind: "finished", outcome: { state: "completed" } },
    { kind: "text", pieces: ["Hi"] },
  ]);
});

test("rows an update doesn't touch keep their identity, so the view skips them", () => {
  const before = apply(EMPTY, [whole("First."), call("c-1"), delta("Wri")]);
  const after = apply(before, [delta("ting")]);

  assert.equal(after.rows[0], before.rows[0]);
  assert.equal(after.rows[1], before.rows[1]);
  assert.notEqual(after.rows[2], before.rows[2]);
});

test("only events with the fields this app reads are taken; unknown ones pass", () => {
  const good: unknown[] = [
    delta("x"),
    whole("x"),
    call("c-1"),
    done("c-1", true),
    { event: "finished", outcome: { state: "failed", code: "run.failed" } },
    { event: "newer" },
  ];
  const bad: unknown[] = [
    undefined,
    null,
    "text",
    {},
    { event: 3 },
    { event: "text_delta" },
    { event: "text", text: 4 },
    { event: "tool_call", call: "c-1" },
    { event: "tool_done", call: "c-1", failed: "no" },
    { event: "finished" },
    { event: "finished", outcome: null },
    { event: "finished", outcome: { state: 1 } },
  ];

  assert.ok(good.every(isRunEvent));
  assert.ok(!bad.some(isRunEvent));
});

test("a reused call id finishes only its newest row, once", () => {
  const { rows } = apply(EMPTY, [call("c-1"), call("c-1"), done("c-1", true), done("c-1", false)]);

  assert.deepEqual(rows, [
    { kind: "tool", call: "c-1", tool: "Read", state: "running" },
    { kind: "tool", call: "c-1", tool: "Read", state: "failed" },
  ]);
});
