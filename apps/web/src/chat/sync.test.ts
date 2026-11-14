import assert from "node:assert/strict";
import { test } from "node:test";

import type { ChatRef } from "../chat-ref.ts";
import type { ChatEntry, Envelope } from "../protocol.ts";
import type { Heard } from "./heard.ts";
import { START, type Step, type Sync, ended, hear, more, open } from "./sync.ts";

const place: ChatRef = { workspace: "w-1", chat: "c-1" };

function ids(from = 10): () => number {
  let next = from;
  return () => next++;
}

const said = (seq: number): ChatEntry => ({ seq, event: { kind: "prompt", request: `q-${seq}`, text: `p${seq}` } });
const range = (from: number, to: number) => Array.from({ length: to - from + 1 }, (_, at) => said(from + at));
const snapshot = (id: number, seq: number, events: ChatEntry[], waiting: number[] = []): Heard => ({
  kind: "snapshot",
  id,
  snapshot: { seq, runs: new Map([["r-1", { state: "running", agent: undefined }]]), waiting, events },
});
const queries = (step: Step) => step.send.map((envelope) => (envelope.type === "chat_query" ? [envelope.id, envelope.query] : envelope));

// Opens the chat and lets its snapshot arrive.
function opened(seq: number, events: ChatEntry[], waiting: number[] = []): Sync {
  const id = ids(1);
  const asked = open(START, place, id);
  return hear(asked.sync, snapshot(1, seq, events, waiting), place, id).sync;
}

test("opening asks for a snapshot, then subscribes after it", () => {
  const id = ids();
  const asked = open(START, place, id);
  const answered = hear(asked.sync, snapshot(10, 5, range(4, 5)), place, id);

  assert.deepEqual(asked.send, [{ id: 10, type: "chat_query", workspace: "w-1", chat: "c-1", query: { kind: "snapshot" } } satisfies Envelope]);
  assert.equal(asked.sync.live, false);
  assert.deepEqual(queries(answered), [[11, { kind: "subscribe", after: 5 }]]);
  assert.equal(answered.sync.live, true);
  assert.equal(answered.sync.chat?.seq, 5);
});

test("a snapshot fetches the events back to the oldest prompt still waiting", () => {
  const id = ids();
  const asked = open(START, place, id);
  const answered = hear(asked.sync, snapshot(10, 20, range(18, 20), [19, 9, 12]), place, id);
  const partly = hear(answered.sync, { kind: "window", id: 12, events: range(14, 17) }, place, id);
  const rest = hear(partly.sync, { kind: "window", id: 13, events: range(9, 13) }, place, id);

  assert.deepEqual(queries(answered), [
    [11, { kind: "subscribe", after: 20 }],
    [12, { kind: "window", before: 18, limit: 9 }],
  ]);
  assert.deepEqual(queries(partly), [[13, { kind: "window", before: 14, limit: 5 }]]);
  assert.deepEqual(queries(rest), []);
  assert.equal(rest.sync.window, undefined);
  assert.equal(rest.sync.chat?.first, 9);
});

test("an empty window stops the fetching", () => {
  const id = ids();
  const answered = hear(open(START, place, id).sync, snapshot(10, 20, range(18, 20), [9]), place, id);
  const empty = hear(answered.sync, { kind: "window", id: 12, events: [] }, place, id);

  assert.deepEqual(queries(empty), []);
  assert.equal(empty.sync.window, undefined);
});

test("events commit while subscribed, and a gap or lost events take a new snapshot", () => {
  const live = opened(3, range(1, 3));
  const id = ids(50);
  const next = hear(live, { kind: "entry", entry: said(4) }, place, id);
  const gap = hear(next.sync, { kind: "entry", entry: said(6) }, place, ids(50));
  const lost = hear(next.sync, { kind: "lost" }, place, ids(50));

  assert.deepEqual(next.send, []);
  assert.equal(next.sync.chat?.seq, 4);
  for (const step of [gap, lost]) {
    assert.deepEqual(queries(step), [[50, { kind: "snapshot" }]]);
    assert.equal(step.sync.live, false);
    // The screen keeps what it shows until the snapshot arrives.
    assert.equal(step.sync.chat, next.sync.chat);
  }
});

test("while a snapshot is on its way, events and lost word are dropped", () => {
  const live = opened(3, range(1, 3));
  const id = ids(50);
  const asking = open(live, place, id).sync;

  const entry = hear(asking, { kind: "entry", entry: said(4) }, place, id);
  const lost = hear(asking, { kind: "lost" }, place, id);
  const unopened = hear(START, { kind: "entry", entry: said(1) }, place, id);

  assert.equal(entry.sync, asking);
  assert.equal(lost.sync, asking);
  assert.deepEqual([...entry.send, ...lost.send, ...unopened.send], []);
  assert.equal(unopened.sync, START);
});

test("answers to queries given up on are dropped", () => {
  const id = ids();
  const first = open(START, place, id);
  const second = open(first.sync, place, id);
  const stale = hear(second.sync, snapshot(10, 9, range(1, 9)), place, id);
  const fresh = hear(second.sync, snapshot(11, 3, range(1, 3), [1]), place, id);
  const scrolled = more(opened(300, range(250, 300)), place, ids(60));
  const reopened = open(scrolled.sync, place, id);
  const late = hear(reopened.sync, { kind: "window", id: 60, events: range(150, 249) }, place, id);

  assert.equal(stale.sync, second.sync);
  assert.equal(fresh.sync.chat?.seq, 3);
  assert.equal(late.sync, reopened.sync);
  assert.equal(late.sync.chat?.first, 250);
});

test("scrolling up asks for one page before the oldest event at a time", () => {
  const live = opened(300, range(250, 300));
  const id = ids(60);
  const first = more(live, place, id);
  const again = more(first.sync, place, id);
  const arrived = hear(first.sync, { kind: "window", id: 60, events: range(150, 249) }, place, id);
  const near = more(opened(30, range(20, 30)), place, id);

  assert.deepEqual(queries(first), [[60, { kind: "window", before: 250, limit: 100 }]]);
  assert.deepEqual(again.send, []);
  assert.equal(arrived.sync.chat?.first, 150);
  assert.deepEqual(queries(near), [[61, { kind: "window", before: 20, limit: 19 }]]);
  for (const sync of [START, opened(3, range(1, 3)), ended(live)]) {
    assert.deepEqual(more(sync, place, id).send, []);
  }
});

test("text streams onto the chat it shows, and the end of the connection keeps it", () => {
  const live = opened(1, range(1, 1));
  const streamed = hear(live, { kind: "run", run: "r-1", event: { event: "text_delta", text: "He" } }, place, ids());
  const unopened = hear(START, { kind: "run", run: "r-1", event: { event: "text_delta", text: "He" } }, place, ids());
  const gone = ended(open(streamed.sync, place, ids()).sync);

  assert.equal(streamed.sync.chat?.live.get("r-1")?.length, 1);
  assert.equal(unopened.sync, START);
  assert.deepEqual(gone, { chat: streamed.sync.chat, snapshot: undefined, window: undefined, live: false });
  assert.equal(ended(gone), gone);
});
