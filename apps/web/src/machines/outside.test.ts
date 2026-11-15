import assert from "node:assert/strict";
import { test } from "node:test";

import type { ChatEntry, Envelope, OutsideSession, SessionsAnswerKind } from "../protocol.ts";
import { lines } from "../chat/lines.ts";
import { fake } from "./fake-socket.ts";
import { MISSING, readOn, sessions } from "./outside.ts";

const SESSION: OutsideSession = {
  agent: "claude",
  session: "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d",
  folder: "/work/app",
  modified_at: 1_700_000_000,
};

const always = () => true;
const said = (seq: number, text: string): ChatEntry => ({ seq, event: { kind: "outside_prompt", text } });
const wrote = (seq: number, text: string): ChatEntry => ({
  seq,
  event: { kind: "run", update: { run: SESSION.session, event: { event: "text", text } } },
});

// Answers each query the core gets with the next of `answers`, as it is sent.
function core(answers: SessionsAnswerKind[]) {
  const machine = fake();
  const asked: Envelope[] = [];
  const answer = () => {
    const query = machine.sent[asked.length];
    const next = answers[asked.length];
    if (query?.type === "sessions_query" && next !== undefined) {
      asked.push(query);
      machine.deliver({ id: query.id, type: "sessions_answer", request: query.request, machine: query.machine, answer: next });
    }
  };
  const send = machine.socket.send;
  machine.socket.send = (envelope) => {
    const sent = send(envelope);
    queueMicrotask(answer);
    return sent;
  };
  return { ...machine, asked };
}

function queries(asked: Envelope[]): unknown[] {
  return asked.map((envelope) => (envelope.type === "sessions_query" ? envelope.query : undefined));
}

test("the list asks the machine and takes its own answer", async () => {
  const machine = core([{ kind: "list", sessions: [SESSION] }]);

  assert.deepEqual(await sessions(machine.socket, "mc-1", machine.later), { ok: true, value: [SESSION] });
  const [query] = machine.asked;
  assert.ok(query?.type === "sessions_query");
  assert.deepEqual({ ...query, request: "r" }, { id: 11, type: "sessions_query", request: "r", machine: "mc-1", query: { kind: "list" } });
  assert.equal(machine.listeners(), 0);
});

test("a session is read page by page, and a refresh reads only what came after", async () => {
  const machine = core([
    { kind: "opened", events: [said(1, "Fix it"), wrote(2, "Looking")], more: true },
    // A repeat is dropped, and the core's skipped number 4 leaves no gap in what is shown.
    { kind: "opened", events: [wrote(2, "Looking"), wrote(3, "Done")], more: false },
    { kind: "opened", events: [said(5, "Thanks")], more: false },
  ]);
  const open = (after: number) => ({ kind: "open", agent: "claude", session: SESSION.session, after });

  const first = await readOn(machine.socket, "mc-1", SESSION, undefined, machine.later, always);
  assert.ok(first.ok);
  const refreshed = await readOn(machine.socket, "mc-1", SESSION, first.value, machine.later, always);
  assert.ok(refreshed.ok);

  assert.deepEqual(queries(machine.asked), [open(0), open(2), open(3)]);
  assert.equal(first.value.after, 3);
  assert.equal(refreshed.value.after, 5);
  const shown = lines(refreshed.value.chat).map((line) => (line.kind === "row" && line.row.kind === "text" ? line.row.pieces.join("") : line.kind));
  assert.deepEqual(shown, ["prompt", "agent", "Looking", "Done", "prompt"]);
  assert.equal(refreshed.value.chat.seq, 4);
});

test("a missing session, a newer answer, an error and an empty page each end the read", async () => {
  const missing = core([{ kind: "missing" }]);
  const newer = core([{ kind: "list", sessions: [] }]);
  const empty = core([{ kind: "opened", events: [], more: true }]);
  const stale = core([
    { kind: "opened", events: [said(1, "Hi")], more: true },
    { kind: "opened", events: [said(1, "Hi")], more: true },
  ]);
  const refused = fake();

  assert.deepEqual(await readOn(missing.socket, "mc-1", SESSION, undefined, missing.later, always), { ok: false, code: MISSING });
  assert.deepEqual(await readOn(newer.socket, "mc-1", SESSION, undefined, newer.later, always), { ok: false, code: "answer.malformed" });
  const read = await readOn(empty.socket, "mc-1", SESSION, undefined, empty.later, always);
  assert.ok(read.ok);
  assert.equal(read.value.after, 0);
  const repeated = await readOn(stale.socket, "mc-1", SESSION, undefined, stale.later, always);
  assert.ok(repeated.ok);
  assert.equal(stale.asked.length, 2);
  const listing = sessions(refused.socket, "mc-1", refused.later);
  refused.deliver({ id: refused.sent[0]?.id ?? 0, type: "error", code: "sessions.not_owner", retryable: false });
  assert.deepEqual(await listing, { ok: false, code: "sessions.not_owner" });
});

test("no more pages are asked for once no one waits for them", async () => {
  const machine = core([
    { kind: "opened", events: [said(1, "Hi")], more: true },
    { kind: "opened", events: [said(2, "Again")], more: false },
  ]);

  const read = await readOn(machine.socket, "mc-1", SESSION, undefined, machine.later, () => false);

  assert.ok(read.ok);
  assert.equal(read.value.after, 1);
  assert.equal(machine.asked.length, 1);
});
