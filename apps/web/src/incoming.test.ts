import assert from "node:assert/strict";
import { test } from "node:test";

import { type Incoming, parse, route } from "./incoming.ts";

const frame = (value: unknown) => JSON.stringify(value);
const app = { name: "weacon-server", version: "0.0.0", commit: "0000000" };
const run = (event: unknown) => frame({ id: 0, type: "run", run: "r-1", event });

test("frames that aren't messages are dropped", () => {
  assert.equal(parse(new ArrayBuffer(1)), undefined);
  assert.equal(parse("not json"), undefined);
  assert.equal(parse("[1]"), undefined);
  assert.equal(parse("null"), undefined);
  assert.equal(parse('{"type":3}'), undefined);
  assert.equal(parse('{"type":"welcome","app":{}}'), undefined);
});

test("only the messages this app acts on pass, each with the fields it reads", () => {
  const good = [
    { id: 1, type: "welcome", app, capabilities: ["run.stream"] },
    { id: 1, type: "welcome", app },
    { id: 4, type: "error", code: "request.conflict", retryable: false },
    { id: 4, type: "receipt", request: "r-1", state: "accepted" },
    { id: 0, type: "run", run: "r-1", event: { event: "text_delta", text: "x" } },
    { id: 5, type: "diff_answer", request: "r-1", workspace: "w-1", chat: "c-1", answer: { kind: "unchanged" } },
  ];
  const bad = [
    { id: 0, type: "newer" },
    { id: 0, type: "log", level: "info", msg: "app started" },
    { type: "receipt", request: "r-1", state: "accepted" },
    { id: 1, type: "welcome", app: { name: "x" } },
    { id: 1, type: "welcome", app, capabilities: [3] },
    { id: 4, type: "error" },
    { id: 4, type: "receipt", state: "accepted" },
    { id: 4, type: "receipt", request: "r-1" },
    { id: 0, type: "run", event: { event: "text_delta", text: "x" } },
    { id: 0, type: "run", run: 7, event: { event: "text_delta", text: "x" } },
    { id: 5, type: "diff_answer", request: "r-1", workspace: "w-1" },
  ];

  for (const message of good) {
    assert.deepEqual(parse(frame(message)), message);
  }
  for (const message of bad) {
    assert.equal(parse(frame(message)), undefined, JSON.stringify(message));
  }
});

test("a run message passes only with the event fields this app reads; unknown events pass", () => {
  const good: unknown[] = [
    { event: "text_delta", text: "x" },
    { event: "text", text: "x" },
    { event: "tool_call", call: "c-1", tool: "Read", input: {} },
    { event: "tool_done", call: "c-1", failed: true },
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

  for (const event of good) {
    assert.notEqual(parse(run(event)), undefined, JSON.stringify(event));
  }
  for (const event of bad) {
    assert.equal(parse(run(event)), undefined, JSON.stringify(event));
  }
});

test("each message goes to the handlers it may be for; an error to both a prompt and the diff query", () => {
  const seen: string[] = [];
  const routes = {
    run: (message: Incoming) => seen.push(`run ${message.type}`),
    answers: (message: Incoming) => seen.push(`answers ${message.type}`),
    changes: (message: Incoming) => seen.push(`changes ${message.type}`),
  };
  const messages: Incoming[] = [
    { id: 0, type: "run", run: "r-1", event: { event: "text_delta", text: "x" } },
    { id: 4, type: "receipt", request: "r-1", state: "accepted" },
    { id: 5, type: "diff_answer", request: "r-2", workspace: "w-1", chat: "c-1", answer: { kind: "unchanged" } },
    { id: 6, type: "error", code: "request.conflict", retryable: false },
    { id: 1, type: "welcome", app },
  ];
  for (const message of messages) {
    route(message, routes);
  }

  assert.deepEqual(seen, ["run run", "answers receipt", "changes diff_answer", "answers error", "changes error"]);
});
