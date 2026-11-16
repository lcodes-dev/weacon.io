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
    { event: "permission_asked", run: "r-1", prompt: 3, tool: "Bash", input: null, wants: { text: "ls" } },
    { event: "permission_asked", run: "r-1", prompt: 3, tool: "Bash", input: null, wants: { text: "ls", cut: true } },
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
    { event: "permission_asked", run: "r-1", prompt: 2, tool: "Bash", wants: "ls" },
    { event: "permission_asked", run: "r-1", prompt: 2, tool: "Bash", wants: { text: { command: "ls" } } },
    { event: "permission_asked", run: "r-1", prompt: 2, tool: "Bash", wants: { text: "ls", cut: "yes" } },
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
    worktree: (message: Incoming) => seen.push(`worktree ${message.type}`),
  };
  const messages: Incoming[] = [
    { id: 0, type: "run", run: "r-1", event: { event: "text_delta", text: "x" } },
    { id: 4, type: "receipt", request: "r-1", state: "accepted" },
    { id: 5, type: "diff_answer", request: "r-2", workspace: "w-1", chat: "c-1", answer: { kind: "unchanged" } },
    { id: 6, type: "error", code: "request.conflict", retryable: false },
    { id: 1, type: "welcome", app },
    { id: 7, type: "chat", workspace: "w-1", chat: "c-1", entry: said },
    { id: 8, type: "chat_answer", workspace: "w-1", chat: "c-1", answer: { kind: "events_lost" } },
    { id: 9, type: "worktree_answer", request: "r-3", workspace: "w-1", chat: "c-1", answer: { kind: "terminal_opened" } },
  ];
  for (const message of messages) {
    route(message, routes);
  }

  assert.deepEqual(seen, ["run run", "answers receipt", "changes diff_answer", "answers error", "changes error", "chat error",
    "worktree error", "chat chat", "chat chat_answer", "worktree worktree_answer"]);
});

test("a chat's events pass with the fields this app reads, and a newer kind keeps its place", () => {
  const good = [
    said.event,
    { kind: "outside_prompt", text: "Hi" },
    { kind: "message", author: "m-1", text: "Hi" },
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
    { kind: "message", text: "Hi" },
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
    { kind: "codex", owner: { kind: "shared", member: "m-2", owner: "m-1" } },
    { kind: "gemini", owner: { kind: "shared", member: "m-2", owner: "m-1" } },
  ];
  const lost = [
    { kind: 4, owner: { kind: "shared", member: "m-2", owner: "m-1" } },
    { kind: "codex", owner: { kind: "shared" } },
    { kind: "codex", owner: { kind: "shared", member: "m-2" } },
    { kind: "claude", owner: { kind: "team" } },
    { kind: "claude", owner: { kind: "member" } },
    { kind: "claude", owner: { kind: "shared", member: "m-2", owner: "m-1" }, machine: 5 },
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

test("a machine's settings answer passes with the fields the page reads, and no other kind does", () => {
  const settings = (answer: unknown) => frame({ id: 6, type: "machine_settings_answer", request: "r-1", machine: "mc-1", workspace: "w-1", answer });
  const current = { kind: "current", access: "full", sign_ins: [{ agent: "claude", state: "signed_in" }] };
  const good = [current, { ...current, sign_ins: [] }, { kind: "repos", repos: ["web"] }, { kind: "agents", agents: ["claude"] }];
  const bad = [
    null,
    { access: "full" },
    { ...current, sign_ins: "none" },
    { ...current, sign_ins: [{ agent: "claude" }] },
    { ...current, sign_ins: [null] },
    { ...current, access: undefined },
    { ...current, access: "sandboxed" },
    { ...current, sign_ins: [{ agent: "claude", state: "expired" }] },
    { kind: "repos" },
    { kind: "agents", agents: [7] },
    { kind: "later" },
  ];

  for (const answer of good) {
    assert.equal(parse(settings(answer))?.type, "machine_settings_answer", JSON.stringify(answer));
  }
  for (const answer of bad) {
    assert.equal(parse(settings(answer)), undefined, JSON.stringify(answer));
  }
  assert.equal(parse(frame({ id: 6, type: "machine_settings_answer", request: "r-1", workspace: "w-1", answer: current })), undefined);
});

test("a terminal update passes with the fields the terminal reads, and no other kind does", () => {
  const update = (event: unknown) => frame({ id: 0, type: "terminal", terminal: "t-1", workspace: "w-1", event });
  const good = [{ kind: "opened" }, { kind: "output", data: "cd" }, { kind: "closed" }, { kind: "closed", code: "terminal.busy" }];
  const bad = [null, { kind: "output" }, { kind: "output", data: 7 }, { kind: "closed", code: 7 }, { kind: "later", data: "cd" }];

  for (const event of good) {
    assert.equal(parse(update(event))?.type, "terminal", JSON.stringify(event));
  }
  for (const event of bad) {
    assert.equal(parse(update(event)), undefined, JSON.stringify(event));
  }
  assert.equal(parse(frame({ id: 0, type: "terminal", terminal: "t-1", event: { kind: "closed" } })), undefined);
});

test("a sessions answer passes with the fields the list and the transcript read, and a newer kind passes to be ignored", () => {
  const sessions = (answer: unknown) => frame({ id: 6, type: "sessions_answer", request: "r-1", machine: "mc-1", answer });
  const session = { agent: "codex", session: "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d", modified_at: 5 };
  const entry = { seq: 1, event: { kind: "outside_prompt", text: "Hi" } };
  const good = [
    { kind: "list", sessions: [session, { ...session, folder: "/work" }] },
    { kind: "opened", events: [entry], more: false },
    { kind: "missing" },
    { kind: "later" },
  ];
  const bad = [
    null,
    {},
    { kind: "list", sessions: "none" },
    { kind: "list", sessions: [{ ...session, folder: 3 }] },
    { kind: "list", sessions: [{ ...session, modified_at: 1.5 }] },
    { kind: "list", sessions: [{ ...session, agent: undefined }] },
    { kind: "list", sessions: [{ ...session, session: undefined }] },
    { kind: "list", sessions: [null] },
    { kind: "opened", events: [entry] },
    { kind: "opened", events: [{ seq: 0, event: entry.event }], more: false },
    { kind: "opened", events: "none", more: false },
  ];

  for (const answer of good) {
    assert.equal(parse(sessions(answer))?.type, "sessions_answer", JSON.stringify(answer));
  }
  for (const answer of bad) {
    assert.equal(parse(sessions(answer)), undefined, JSON.stringify(answer));
  }
  assert.equal(parse(frame({ id: 6, type: "sessions_answer", machine: "mc-1", answer: { kind: "missing" } })), undefined);
  assert.equal(parse(frame({ id: 6, type: "sessions_answer", request: "r-1", answer: { kind: "missing" } })), undefined);
});

test("a symbols answer passes only with each symbol's fields", () => {
  const symbols = (answer: unknown) => frame({ id: 6, type: "symbols_answer", request: "r-1", machine: "mc-1", workspace: "w-1", answer });
  const symbol = { repo: "site", kind: "fn", name: "a::run", path: "src/lib.rs", line: 2 };
  const good = [{ kind: "found", symbols: [symbol], more: true }, { kind: "found", symbols: [], more: false }];
  const bad = [
    null,
    {},
    { kind: "later" },
    { kind: "found", symbols: [{ ...symbol, kind: "class" }], more: false },
    { kind: "found", symbols: "none", more: false },
    { kind: "found", symbols: [symbol] },
    ...["repo", "kind", "name", "path"].map((key) => ({ kind: "found", symbols: [{ ...symbol, [key]: 1 }], more: false })),
    { kind: "found", symbols: [{ ...symbol, line: 1.5 }], more: false },
    { kind: "found", symbols: [{ ...symbol, line: 0 }], more: false },
    { kind: "found", symbols: [{ ...symbol, line: -1 }], more: false },
    { kind: "found", symbols: [null], more: false },
  ];

  for (const answer of good) {
    assert.equal(parse(symbols(answer))?.type, "symbols_answer", JSON.stringify(answer));
  }
  for (const answer of bad) {
    assert.equal(parse(symbols(answer)), undefined, JSON.stringify(answer));
  }
  for (const missing of ["request", "machine", "workspace"]) {
    const fields: Record<string, unknown> = { id: 6, type: "symbols_answer", request: "r-1", machine: "mc-1", workspace: "w-1", answer: good[0] };
    delete fields[missing];
    assert.equal(parse(frame(fields)), undefined, missing);
  }
});

test("a diff answer passes only with every field the diff view reads", () => {
  const file = (path: string) => ({
    path,
    change: { kind: "modified" },
    content: { kind: "text", hunks: [{ old_start: 3, new_start: 5, lines: [{ op: "added", text: "d", no_newline: true }] }] },
  });
  const answer = (body: unknown) =>
    JSON.stringify({ id: 0, type: "diff_answer", request: "q-1", workspace: "w-1", chat: "c-1", answer: body });
  const text = (hunks: unknown) => ({ kind: "file", files: 1, file: { path: "a", change: { kind: "added" }, content: { kind: "text", hunks } } });
  const malformed = [
    "nope",
    null,
    { kind: "file", files: "1", file: file("a.rs") },
    { kind: "file", files: 1.5, file: file("a.rs") },
    { kind: "file", files: 0, file: file("a.rs") },
    { kind: "file", files: 1, file: { ...file("a.rs"), path: 7 } },
    { kind: "file", files: 1, file: { ...file("a.rs"), change: { kind: "renamed" } } },
    { kind: "file", files: 1, file: { ...file("a.rs"), change: { kind: "copied" } } },
    { kind: "file", files: 1, file: { ...file("a.rs"), content: { kind: "symlink" } } },
    text([{ old_start: 1, new_start: 1, lines: [{ op: "moved", text: "x" }] }]),
    { kind: "file", files: 1, file: { ...file("a.rs"), change: "added" } },
    { kind: "file", files: 1, file: { ...file("a.rs"), content: { hunks: [] } } },
    text("none"),
    text([{ old_start: 1, new_start: "1", lines: [] }]),
    text([{ old_start: "1", new_start: 1, lines: [] }]),
    text([{ old_start: 1, new_start: 1, lines: "none" }]),
    text([{ old_start: 1, new_start: 1, lines: [{ op: "added" }] }]),
    text([{ old_start: 1, new_start: 1, lines: [{ text: "x" }] }]),
    text([{ old_start: 1, new_start: 1, lines: [null] }]),
  ];

  const good = [
    { kind: "unchanged" },
    { kind: "file", files: 2, file: file("a.rs") },
    { kind: "file", files: 1, file: { ...file("b.rs"), change: { kind: "renamed", from: "a.rs" } } },
  ];

  for (const body of good) {
    assert.notEqual(parse(answer(body)), undefined, JSON.stringify(body));
  }
  for (const body of malformed) {
    assert.equal(parse(answer(body)), undefined, JSON.stringify(body));
  }
});

test("a worktree answer passes only with the fields the review bar reads", () => {
  const worktree = (answer: unknown) => frame({ id: 6, type: "worktree_answer", request: "r-1", workspace: "w-1", chat: "c-1", answer });
  const state = { kind: "state", removed: false, uncommitted: true, unpushed: false };
  const good = [state, { kind: "pull_request_opened", url: "https://example.com/pr/1" }, { kind: "pull_request_page", url: "https://example.com/compare" }, { kind: "terminal_opened" }];
  const bad = [null, { kind: "later" }, { removed: false }, { ...state, removed: "no" }, { ...state, unpushed: undefined }, { kind: "pull_request_page" }, { kind: "pull_request_opened", url: 3 }, { kind: "pull_request_page", url: "javascript:alert(1)" }, { kind: "pull_request_opened", url: "http://example.com/pr/1" }, { kind: "pull_request_page", url: "not a url" }];

  for (const answer of good) {
    assert.equal(parse(worktree(answer))?.type, "worktree_answer", JSON.stringify(answer));
  }
  for (const answer of bad) {
    assert.equal(parse(worktree(answer)), undefined, JSON.stringify(answer));
  }
  assert.equal(parse(frame({ id: 6, type: "worktree_answer", workspace: "w-1", chat: "c-1", answer: state })), undefined);
});

test("a run's live update is read with its chat and a run event, and nothing else is", () => {
  const live = (update: unknown) => frame({ id: 0, type: "chat_live", workspace: "w-1", chat: "c-1", update });
  const event = { event: "text_delta", text: "Wor" };

  assert.deepEqual(parse(live({ run: "r-1", event })), { id: 0, type: "chat_live", workspace: "w-1", chat: "c-1", update: { run: "r-1", event } });
  assert.equal(parse(live({ run: "r-1" })), undefined);
  assert.equal(parse(live({ event })), undefined);
  assert.equal(parse(live("r-1")), undefined);
  const stamped = (after: unknown) => frame({ id: 0, type: "chat_live", workspace: "w-1", chat: "c-1", update: { run: "r-1", event }, after });
  assert.equal(parse(stamped(3))?.type, "chat_live");
  assert.equal(parse(stamped("3")), undefined);
  assert.equal(parse(frame({ id: 0, type: "chat_live", chat: "c-1", update: { run: "r-1", event } })), undefined);
});

test("a prompt keeps a well-formed pick note and loses a malformed one", () => {
  const prompt = (pick: unknown) => JSON.stringify({ id: 0, type: "chat", workspace: "w-1", chat: "c-1", entry: { seq: 1, event: { kind: "prompt", request: "q-1", text: "Hi", pick } } });
  const note = { url: "http://localhost:3000/", title: "Home", selector: "p" };
  const event = (data: string) => {
    const message = parse(data);
    return message?.type === "chat" ? message.entry.event : undefined;
  };

  assert.deepEqual(event(prompt(note)), { kind: "prompt", request: "q-1", text: "Hi", pick: note });
  assert.deepEqual(event(prompt({ url: "http://localhost:3000/" })), { kind: "prompt", request: "q-1", text: "Hi" });
});
