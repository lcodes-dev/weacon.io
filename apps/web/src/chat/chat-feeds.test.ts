import assert from "node:assert/strict";
import { test } from "node:test";

import type { Incoming } from "../incoming.ts";
import type { Envelope } from "../protocol.ts";
import { ChatFeeds } from "./chat-feeds.ts";
import type { Sync } from "./sync.ts";

const REF = { workspace: "w-1", chat: "c-1" };

// A socket that records what is sent, hands out envelope ids, and counts its connections.
function socket() {
  const sent: Envelope[] = [];
  const handlers = new Set<(message: Incoming) => void>();
  let next = 0;
  const feed = {
    connection: 1,
    nextId: () => ++next,
    send: (envelope: Envelope) => sent.push(envelope) > 0,
    subscribe: (handle: (message: Incoming) => void) => {
      handlers.add(handle);
      return () => handlers.delete(handle);
    },
  };
  const hear = (message: Incoming) => [...handlers].forEach((handle) => handle(message));
  // Each flush runs at once, so what is shown follows each message.
  const feeds = new ChatFeeds(feed, () => (flush) => flush());
  const queries = (kind: string) => sent.filter((envelope) => envelope.type === "chat_query" && envelope.query.kind === kind);
  return { feed, feeds, sent, hear, queries, handlers };
}

const snapshot = (id: number): Incoming => ({
  id,
  type: "chat_answer",
  ...REF,
  answer: { kind: "snapshot", seq: 1, runs: [{ run: "r-1", state: "running" }], waiting: [], events: [{ seq: 1, event: { kind: "prompt", request: "q-1", text: "Hi" } }] },
});
const delta = (text: string): Incoming => ({ id: 0, type: "run", run: "r-1", event: { event: "text_delta", text } });
const entry = (seq: number): Incoming => ({ id: 0, type: "chat", ...REF, entry: { seq, event: { kind: "prompt", request: `q-${seq}`, text: "More" } } });

function streamed(sync: Sync | undefined): string[] {
  return (sync?.chat?.items ?? []).flatMap((item) => (item.kind === "turn" ? item.turn.transcript.rows.flatMap((row) => (row.kind === "text" ? row.pieces : [])) : []));
}

test("a chat shown again on the same connection keeps its one subscription, and what it showed", () => {
  const { feeds, hear, queries } = socket();
  const first: Sync[] = [];
  const { hide } = feeds.show(REF, (sync) => first.push(sync));
  hear(snapshot(1));
  hide();

  const again: Sync[] = [];
  feeds.show(REF, (sync) => again.push(sync));

  assert.equal(queries("snapshot").length, 1);
  assert.equal(queries("subscribe").length, 1);
  assert.equal(again[0]?.chat?.seq, 1);
  assert.equal(again[0]?.state.kind, "live");
});

test("a chat no screen shows takes its log's entries but drops its runs' updates", () => {
  const { feeds, hear } = socket();
  const shown: Sync[] = [];
  const { hide } = feeds.show(REF, (sync) => shown.push(sync));
  hear(snapshot(1));
  hear(delta("He"));
  hide();
  hear(delta("llo"));
  hear(entry(2));

  const again: Sync[] = [];
  feeds.show(REF, (sync) => again.push(sync));

  assert.deepEqual(streamed(again[0]), ["He"]);
  assert.equal(again[0]?.chat?.seq, 2);
  assert.equal(shown.at(-1)?.chat?.seq, 1);
});

test("on a new connection a chat asks again, keeping what it showed, and the old one stops listening", () => {
  const { feed, feeds, hear, queries, handlers } = socket();
  const { hide } = feeds.show(REF, () => {});
  hear(snapshot(1));
  hide();
  feed.connection = 2;

  const again: Sync[] = [];
  feeds.show(REF, (sync) => again.push(sync));

  assert.equal(queries("snapshot").length, 2);
  assert.equal(handlers.size, 1);
  assert.equal(again[0]?.chat?.seq, 1);
  assert.equal(again[0]?.state.kind, "asking");
});

test("a chat whose ask failed asks again when it is shown", () => {
  const { feeds, hear, queries } = socket();
  const { hide } = feeds.show(REF, () => {});
  hear({ id: 1, type: "error", code: "chat.not_found", retryable: false });
  hide();

  feeds.show(REF, () => {});

  assert.equal(queries("snapshot").length, 2);
});
