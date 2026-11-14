import assert from "node:assert/strict";
import { test } from "node:test";

import type { Place } from "../place.ts";
import type { Envelope } from "../protocol.ts";
import { about } from "./parse.ts";

const place: Place = { workspace: "w-1", chat: "c-1" };

// A frame as the server might send it, checked only by `about`.
const frame = (fields: Record<string, unknown>) => ({ id: 7, workspace: "w-1", chat: "c-1", ...fields }) as unknown as Envelope;
const update = (entry: unknown) => frame({ type: "chat", entry });
const answer = (body: unknown) => frame({ type: "chat_answer", answer: body });
const said = { seq: 1, event: { kind: "prompt", request: "q-1", text: "Hi" } };

test("committed events of this chat are heard, with a place in the log", () => {
  const goodEvents = [
    said.event,
    { kind: "receipt", receipt: { request: "q-1", state: "accepted" } },
    { kind: "run", update: { run: "r-1", event: { event: "text", text: "Hello." } } },
    { kind: "run_status", status: { run: "r-1", state: "idle" } },
    { kind: "newer", anything: 1 },
  ];
  const badEvents = [
    undefined,
    { kind: 3 },
    { kind: "prompt", text: "Hi" },
    { kind: "receipt", receipt: {} },
    { kind: "run", update: { event: { event: "text", text: "x" } } },
    { kind: "run", update: { run: "r-1", event: { event: "text" } } },
    { kind: "run_status", status: { run: "r-1" } },
  ];

  for (const event of goodEvents) {
    assert.deepEqual(about(update({ seq: 3, event }), place), { kind: "entry", entry: { seq: 3, event } });
  }
  for (const event of badEvents) {
    assert.equal(about(update({ seq: 3, event }), place), undefined, JSON.stringify(event));
  }
  for (const seq of [0, -1, 1.5, "1", undefined]) {
    assert.equal(about(update({ ...said, seq }), place), undefined, String(seq));
  }
  assert.equal(about(update(null), place), undefined);
});

test("another chat's or workspace's messages aren't heard", () => {
  const other = [
    { ...update(said), chat: "c-2" },
    { ...update(said), workspace: "w-2" },
    { ...answer({ kind: "events_lost" }), chat: "c-2" },
  ] as Envelope[];
  const unrelated = frame({ type: "receipt", request: "q-1", state: "accepted" });

  for (const envelope of [...other, unrelated]) {
    assert.equal(about(envelope, place), undefined);
  }
});

test("a run's status keeps the well-formed parts of who runs it", () => {
  const statusOf = (agent: unknown) => {
    const heard = about(update({ seq: 2, event: { kind: "run_status", status: { run: "r-1", state: "running", agent } } }), place);
    return heard?.kind === "entry" && heard.entry.event.kind === "run_status" ? heard.entry.event.status : undefined;
  };

  assert.deepEqual(statusOf({ kind: "claude", owner: { kind: "member", member: "m-1" }, machine: "laptop-1" }), {
    run: "r-1",
    state: "running",
    agent: { kind: "claude", owner: { kind: "member", member: "m-1" }, machine: "laptop-1" },
  });
  assert.deepEqual(statusOf({ kind: "codex", owner: { kind: "shared" } }), {
    run: "r-1",
    state: "running",
    agent: { kind: "codex", owner: { kind: "shared" } },
  });
  assert.deepEqual(statusOf({ kind: 4, owner: { kind: "robot" }, machine: 5 }), { run: "r-1", state: "running", agent: {} });
  assert.deepEqual(statusOf({ owner: { kind: "member" } }), { run: "r-1", state: "running", agent: {} });
  assert.deepEqual(statusOf(undefined), { run: "r-1", state: "running" });
});

test("answers are heard under the envelope they answer", () => {
  const snapshot = {
    kind: "snapshot",
    seq: 4,
    runs: [{ run: "r-1", state: "waiting", agent: { kind: "claude" } }, { run: 3 }],
    waiting: [3, 0, "x"],
    events: [said],
  };

  assert.deepEqual(about(answer({ kind: "events_lost" }), place), { kind: "lost" });
  assert.deepEqual(about(answer({ kind: "window", events: [said] }), place), { kind: "window", id: 7, events: [said] });
  assert.deepEqual(about(answer(snapshot), place), {
    kind: "snapshot",
    id: 7,
    snapshot: { seq: 4, runs: new Map([["r-1", { state: "waiting", agent: { kind: "claude" } }]]), waiting: [3], events: [said] },
  });
  const broken = [
    { kind: "window", events: [said, { seq: 0 }] },
    { kind: "window" },
    { ...snapshot, seq: -1 },
    { ...snapshot, runs: undefined },
    { ...snapshot, waiting: undefined },
    { ...snapshot, events: "x" },
    { kind: "newer" },
    [],
    null,
  ];
  for (const body of broken) {
    assert.equal(about(answer(body), place), undefined, JSON.stringify(body));
  }
});

test("run updates are heard whatever chat they belong to, when well-formed", () => {
  const text = { event: "text_delta", text: "He" };

  assert.deepEqual(about(frame({ type: "run", run: "r-1", event: text }), place), { kind: "run", run: "r-1", event: text });
  assert.equal(about(frame({ type: "run", run: 1, event: text }), place), undefined);
  assert.equal(about(frame({ type: "run", run: "r-1", event: { event: "text_delta" } }), place), undefined);
  const asked = { event: "permission_asked", run: "r-1", prompt: 2, tool: "Bash", input: {} };
  const withdrawn = { event: "permission_withdrawn", run: "r-1", prompt: 2 };
  const bare = { event: "permission_asked", run: "r-1", prompt: 3, tool: "Bash", input: null };
  for (const event of [asked, bare, withdrawn]) {
    assert.deepEqual(about(frame({ type: "run", run: "r-1", event }), place), { kind: "run", run: "r-1", event });
  }
  const broken = [
    { event: "permission_asked", run: "r-1", prompt: 2 },
    { event: "permission_asked", run: "r-1", prompt: 1.5, tool: "Bash" },
    { event: "permission_withdrawn", prompt: 2 },
  ];
  for (const event of broken) {
    assert.equal(about(frame({ type: "run", run: "r-1", event }), place), undefined, JSON.stringify(event));
    const entry = { seq: 1, event: { kind: "run", update: { run: "r-1", event } } };
    assert.equal(about(update(entry), place), undefined, JSON.stringify(event));
  }
});
