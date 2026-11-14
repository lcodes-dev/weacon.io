import assert from "node:assert/strict";
import { test } from "node:test";

import { type Incoming, parse, route } from "./incoming.ts";

const frame = (value: unknown) => JSON.stringify(value);
const app = { name: "weacon-server", version: "0.0.0", commit: "0000000" };
const run = (event: unknown) => frame({ id: 0, type: "run", run: "r-1", event });
const said = { seq: 1, event: { kind: "prompt" as const, request: "q-1", text: "Hi" } };
const entry = (event: unknown, seq: unknown = 3) => frame({ id: 7, type: "chat", workspace: "w-1", chat: "c-1", entry: { seq, event } });
const answer = (body: unknown) => frame({ id: 7, type: "chat_answer", workspace: "w-1", chat: "c-1", answer: body });

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
    { id: 0, type: "workspace", workspace: "w-1", seq: 3, change: { kind: "chat_changed", chat: "c-1" } },
    { id: 0, type: "workspace", workspace: "w-1", seq: 4, change: { kind: "member_joined", member: "m-1" } },
    { id: 0, type: "workspace", workspace: "w-1", seq: 5, change: { kind: "newer" } },
  ];
  const bad = [
    { id: 0, type: "constructor" },
    { id: 0, type: "toString" },
    { id: 0, type: "workspace", seq: 3, change: { kind: "chat_changed", chat: "c-1" } },
    { id: 0, type: "workspace", workspace: "w-1", seq: 1.5, change: { kind: "chat_changed", chat: "c-1" } },
    { id: 0, type: "workspace", workspace: "w-1", seq: 3, change: { kind: "chat_changed" } },
    { id: 0, type: "workspace", workspace: "w-1", seq: 3, change: null },
    { id: 0, type: "workspace", workspace: "w-1", seq: 3, change: { kind: 1 } },
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
    { event: "permission_asked", run: "r-1", prompt: 2, tool: "Bash", input: {} },
    { event: "permission_asked", run: "r-1", prompt: 3, tool: "Bash", input: null },
    { event: "permission_withdrawn", run: "r-1", prompt: 2 },
    { event: "permission_answered", answer: { run: "r-1", prompt: 2, allow: false } },
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
    { event: "permission_asked", run: "r-1", prompt: 2 },
    { event: "permission_asked", run: "r-1", prompt: 1.5, tool: "Bash" },
    { event: "permission_withdrawn", prompt: 2 },
    { event: "permission_answered" },
    { event: "permission_answered", answer: { prompt: 2, allow: true } },
    { event: "permission_answered", answer: { run: "r-1", prompt: -1, allow: true } },
    { event: "permission_answered", answer: { run: "r-1", prompt: 2, allow: "yes" } },
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
    chat: (message: Incoming) => seen.push(`chat ${message.type}`),
  };
  const messages: Incoming[] = [
    { id: 0, type: "run", run: "r-1", event: { event: "text_delta", text: "x" } },
    { id: 4, type: "receipt", request: "r-1", state: "accepted" },
    { id: 5, type: "diff_answer", request: "r-2", workspace: "w-1", chat: "c-1", answer: { kind: "unchanged" } },
    { id: 6, type: "error", code: "request.conflict", retryable: false },
    { id: 1, type: "welcome", app },
    { id: 7, type: "chat", workspace: "w-1", chat: "c-1", entry: said },
    { id: 8, type: "chat_answer", workspace: "w-1", chat: "c-1", answer: { kind: "events_lost" } },
  ];
  for (const message of messages) {
    route(message, routes);
  }

  assert.deepEqual(seen, ["run run", "answers receipt", "changes diff_answer", "answers error", "changes error", "chat error",
    "chat chat", "chat chat_answer"]);
});

test("a chat's events pass with the fields this app reads, and a newer kind keeps its place", () => {
  const good = [
    said.event,
    { kind: "outside_prompt", text: "Hi" },
    { kind: "receipt", receipt: { request: "q-1", state: "accepted" } },
    { kind: "run", update: { run: "r-1", event: { event: "text", text: "Hello." } } },
    { kind: "run_status", status: { run: "r-1", state: "idle" } },
    { kind: "newer", anything: 1 },
  ];
  const bad = [
    undefined,
    { kind: 3 },
    { kind: "prompt", text: "Hi" },
    { kind: "outside_prompt" },
    { kind: "receipt", receipt: {} },
    { kind: "run", update: { event: { event: "text", text: "x" } } },
    { kind: "run", update: { run: "r-1", event: { event: "text" } } },
    { kind: "run", update: { run: "r-1", event: { event: "permission_asked", run: "r-1", prompt: 2 } } },
    { kind: "run_status", status: { run: "r-1" } },
  ];

  for (const event of good) {
    assert.notEqual(parse(entry(event)), undefined, JSON.stringify(event));
  }
  for (const event of bad) {
    assert.equal(parse(entry(event)), undefined, JSON.stringify(event));
  }
  for (const seq of [0, -1, 1.5, "1", null]) {
    assert.equal(parse(entry(said.event, seq)), undefined, String(seq));
  }
  assert.equal(parse(frame({ id: 7, type: "chat", workspace: "w-1", chat: "c-1", entry: { event: said.event } })), undefined);
  assert.equal(parse(frame({ id: 7, type: "chat", chat: "c-1", entry: said })), undefined);
  assert.equal(parse(frame({ id: 7, type: "chat", workspace: "w-1", chat: "c-1", entry: null })), undefined);
});

test("a run's status keeps a well-formed agent and drops any other", () => {
  const agentOf = (agent: unknown) => {
    const message = parse(entry({ kind: "run_status", status: { run: "r-1", state: "running", agent } }));
    return message?.type === "chat" && message.entry.event.kind === "run_status" ? message.entry.event.status.agent : "dropped";
  };
  const kept = [
    { kind: "claude", owner: { kind: "member", member: "m-1" }, machine: "Ada's laptop" },
    { kind: "codex", owner: { kind: "shared" } },
    { kind: "gemini", owner: { kind: "shared" } },
  ];
  const lost = [
    { kind: 4, owner: { kind: "shared" } },
    { kind: "claude", owner: { kind: "team" } },
    { kind: "claude", owner: { kind: "member" } },
    { kind: "claude", owner: { kind: "shared" }, machine: 5 },
    { kind: "claude" },
    "claude",
  ];

  for (const agent of kept) {
    assert.deepEqual(agentOf(agent), agent);
  }
  for (const agent of lost) {
    assert.equal(agentOf(agent), undefined, JSON.stringify(agent));
  }
  assert.equal(agentOf(undefined), undefined);
});

test("answers about a chat pass with the fields this app reads", () => {
  const snapshot = { kind: "snapshot", seq: 4, runs: [{ run: "r-1", state: "waiting" }], waiting: [3], events: [said] };
  const good = [{ kind: "events_lost" }, { kind: "window", events: [said] }, { kind: "window", events: [] }, snapshot];
  const bad = [
    { kind: "window", events: [said, { seq: 0 }] },
    { kind: "window" },
    { ...snapshot, seq: -1 },
    { ...snapshot, runs: undefined },
    { ...snapshot, runs: [{ run: 3 }] },
    { ...snapshot, waiting: undefined },
    { ...snapshot, waiting: [0] },
    { ...snapshot, events: "x" },
    { kind: "newer" },
    [],
    null,
  ];

  for (const body of good) {
    assert.notEqual(parse(answer(body)), undefined, JSON.stringify(body));
  }
  for (const body of bad) {
    assert.equal(parse(answer(body)), undefined, JSON.stringify(body));
  }
});

test("a machine's settings answer passes with the fields the page reads, and a newer kind passes to be ignored", () => {
  const settings = (answer: unknown) => frame({ id: 6, type: "machine_settings_answer", request: "r-1", machine: "mc-1", workspace: "w-1", answer });
  const current = { kind: "current", access: "full", team: true, sign_ins: [{ agent: "claude", state: "signed_in" }] };
  const good = [current, { ...current, reports: false }, { ...current, sign_ins: [] }, { kind: "later" }];
  const bad = [
    null,
    { access: "full" },
    { ...current, team: "yes" },
    { ...current, reports: "on" },
    { ...current, sign_ins: "none" },
    { ...current, sign_ins: [{ agent: "claude" }] },
    { ...current, sign_ins: [null] },
    { ...current, access: undefined },
  ];

  for (const answer of good) {
    assert.equal(parse(settings(answer))?.type, "machine_settings_answer", JSON.stringify(answer));
  }
  for (const answer of bad) {
    assert.equal(parse(settings(answer)), undefined, JSON.stringify(answer));
  }
  assert.equal(parse(frame({ id: 6, type: "machine_settings_answer", request: "r-1", workspace: "w-1", answer: current })), undefined);
});
