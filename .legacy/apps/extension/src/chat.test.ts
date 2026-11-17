import assert from "node:assert/strict";
import { test } from "node:test";

import { type Chat, commit, fromSnapshot, lines, missing, older, working } from "./chat.ts";
import type { ChatEntry, RunEvent } from "./protocol.ts";

const run = (seq: number, event: RunEvent): ChatEntry => ({ seq, event: { kind: "run", update: { run: "run-1", event } } });
const status = (seq: number, state: "running" | "waiting" | "idle"): ChatEntry => ({ seq, event: { kind: "run_status", status: { run: "run-1", state } } });
const asked = (seq: number, prompt: number): ChatEntry => run(seq, { event: "permission_asked", run: "run-1", prompt, tool: "Bash", input: null });
const pick = { url: "http://localhost:3000/", title: "Home", selector: "main > button" };

function chat(events: ChatEntry[], waiting: number[] = [], seq = events.at(-1)?.seq ?? 0): Chat {
  return fromSnapshot({ seq, runs: [{ run: "run-1", state: "running" }], waiting, events });
}

test("a turn shows the prompt, its note, the agent's text and its tools by name", () => {
  const shown = chat([
    { seq: 1, event: { kind: "prompt", request: "r-1", text: "Make it blue", pick } },
    { seq: 2, event: { kind: "receipt", receipt: { request: "r-1", state: "accepted" } } },
    status(3, "running"),
    run(4, { event: "started", session: "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d" }),
    run(5, { event: "tool_call", call: "t1", tool: "Edit", input: { file: "x" } }),
    run(6, { event: "tool_call", call: "t2", tool: "Bash", input: {} }),
    run(7, { event: "tool_done", call: "t1", failed: false }),
    run(8, { event: "tool_done", call: "t2", failed: true }),
    run(9, { event: "text", text: "Done." }),
    run(10, { event: "finished", outcome: { state: "completed" } }),
    { seq: 11, event: { kind: "outside_prompt", text: "By hand" } },
  ]);

  assert.deepEqual(lines(shown), [
    { key: "p1", kind: "prompt", text: "Make it blue", pick },
    { key: "ct1", kind: "tool", tool: "Edit", state: "done" },
    { key: "ct2", kind: "tool", tool: "Bash", state: "failed" },
    { key: "x9", kind: "text", text: "Done." },
    { key: "p11", kind: "prompt", text: "By hand" },
  ]);
});

test("a prompt carries what its tool wants, when its event says", () => {
  const wants = { text: "cargo test", cut: true };
  const said = run(2, { event: "permission_asked", run: "run-1", prompt: 1, tool: "Bash", input: null, wants });

  assert.deepEqual(lines(chat([status(1, "waiting"), said], [2])), [{ key: "a2", kind: "ask", run: "run-1", prompt: 1, tool: "Bash", wants, state: "open" }]);
  assert.deepEqual(lines(chat([status(1, "waiting"), asked(2, 1)], [2])), [{ key: "a2", kind: "ask", run: "run-1", prompt: 1, tool: "Bash", state: "open" }]);
});

test("a prompt is open only while the log says it waits, and shows how it was answered", () => {
  const events = [asked(1, 1), asked(2, 2), asked(3, 3), asked(4, 4)];
  const snapshot = chat(events, [2, 3, 4]);
  const later = [
    run(5, { event: "permission_answered", answer: { run: "run-1", prompt: 2, allow: true } }),
    run(6, { event: "permission_answered", answer: { run: "run-1", prompt: 3, allow: false } }),
    run(7, { event: "permission_withdrawn", run: "run-1", prompt: 4 }),
    asked(8, 5),
  ].reduce((at: Chat | undefined, entry) => at && commit(at, entry), snapshot);

  const states = (of: Chat | undefined) => lines(of!).map((line) => (line.kind === "ask" ? line.state : line.kind));
  assert.deepEqual(states(snapshot), ["closed", "open", "open", "open"]);
  assert.deepEqual(states(later), ["closed", "allowed", "denied", "closed", "open"]);
  assert.deepEqual(states(commit(later!, status(9, "running"))), ["closed", "allowed", "denied", "closed", "closed"]);
  assert.deepEqual(states(commit(later!, status(9, "waiting"))), ["closed", "allowed", "denied", "closed", "open"]);
  assert.deepEqual(states(commit(later!, run(9, { event: "finished", outcome: { state: "completed" } }))), ["closed", "allowed", "denied", "closed", "closed"]);
});

test("a failed or stopped turn says so, as does a refused command", () => {
  const ended = chat([
    { seq: 1, event: { kind: "receipt", receipt: { request: "r-1", state: "failed", code: "chat.empty_prompt" } } },
    run(2, { event: "finished", outcome: { state: "failed", code: "agent.signed_out" } }),
    run(3, { event: "finished", outcome: { state: "interrupted" } }),
    run(4, { event: "finished", outcome: { state: "unknown" } }),
    { seq: 5, event: { kind: "receipt", receipt: { request: "r-2", state: "completed" } } },
  ]);

  assert.deepEqual(lines(ended), [
    { key: "e1", kind: "ended", code: "chat.empty_prompt" },
    { key: "e2", kind: "ended", code: "agent.signed_out" },
    { key: "e3", kind: "ended", code: "run.interrupted" },
  ]);
});

test("only the next event is committed; a repeat changes nothing and a gap is refused", () => {
  const shown = chat([status(1, "running")]);

  assert.equal(commit(shown, status(1, "idle")), shown);
  assert.equal(commit(shown, status(3, "idle")), undefined);
  const next = commit(shown, status(2, "idle"));
  assert.equal(next?.seq, 2);
  assert.equal(next?.runs.get("run-1"), "idle");
  assert.equal(commit(next!, run(3, { event: "text", text: "x" }))?.runs, next?.runs);
});

test("a snapshot keeps its latest events without a gap, and older ones join before them", () => {
  const shown = chat([status(9, "running"), status(7, "running"), status(8, "running")], [3], 9);

  assert.deepEqual([shown.first, shown.entries.map((entry) => entry.seq)], [7, [7, 8, 9]]);
  assert.equal(missing(shown), 3);
  const earlier = older(shown, [asked(3, 1), status(5, "running"), status(6, "running"), status(4, "running"), status(8, "idle")]);
  assert.deepEqual([earlier.first, earlier.entries.map((entry) => entry.seq)], [3, [3, 4, 5, 6, 7, 8, 9]]);
  assert.equal(missing(earlier), undefined);
  assert.equal(older(shown, [status(5, "running")]), shown);
  assert.equal(chat([], [], 4).first, 5);
});

test("a chat is working while a run is on a turn", () => {
  const on = (state: "starting" | "running" | "waiting" | "idle" | "sleeping") => working(fromSnapshot({ seq: 0, runs: [{ run: "r", state }], waiting: [], events: [] }));

  assert.deepEqual(["starting", "running", "waiting", "idle", "sleeping"].map((state) => on(state as "idle")), [true, true, true, false, false]);
});
