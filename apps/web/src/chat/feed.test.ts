import assert from "node:assert/strict";
import { test } from "node:test";

import type { Envelope } from "../protocol.ts";
import { ChatFeed } from "./feed.ts";
import type { Sync } from "./sync.ts";

function wired() {
  const sent: Envelope[] = [];
  const shown: Sync[] = [];
  const flushes: (() => void)[] = [];
  let next = 1;
  const feed = new ChatFeed(
    { workspace: "w-1", chat: "c-1" },
    {
      id: () => next++,
      send: (envelope) => sent.push(envelope),
      schedule: (flush) => flushes.push(flush),
      show: (sync) => shown.push(sync),
    },
  );
  return { feed, sent, shown, flushes };
}

const answer = (id: number, seq: number) =>
  ({
    id,
    type: "chat_answer",
    workspace: "w-1",
    chat: "c-1",
    answer: { kind: "snapshot", seq, runs: [{ run: "r-1", state: "running" }], waiting: [], events: [] },
  }) satisfies Envelope;
const delta = (text: string) => ({ id: 0, type: "run", run: "r-1", event: { event: "text_delta", text } }) satisfies Envelope;

test("a burst of messages is shown once, as it stands at the flush", () => {
  const { feed, sent, shown, flushes } = wired();
  feed.open();
  assert.equal(feed.push(answer(1, 4)), true);
  feed.push(delta("He"));
  feed.push(delta("llo"));

  assert.deepEqual(
    sent.map((envelope) => envelope.type === "chat_query" && envelope.query.kind),
    ["snapshot", "subscribe"],
  );
  assert.equal(flushes.length, 1);
  assert.equal(shown.length, 0);
  flushes[0]?.();
  assert.equal(shown.length, 1);
  assert.equal(shown[0]?.chat?.live.get("r-1")?.length, 2);

  feed.push(delta("!"));
  assert.equal(flushes.length, 2);
});

test("what changes nothing schedules nothing, and other messages aren't taken", () => {
  const { feed, sent, flushes } = wired();
  const receipt = { id: 3, type: "receipt", request: "q-1", state: "accepted" } satisfies Envelope;

  assert.equal(feed.push(receipt), false);
  feed.more();
  feed.end();
  assert.deepEqual([sent, flushes], [[], []]);
});

test("the end of the connection keeps the chat but drops what was asked", () => {
  const { feed, shown, flushes } = wired();
  feed.open();
  feed.push(answer(1, 4));
  feed.end();
  feed.push(answer(1, 9));
  flushes[0]?.();

  assert.equal(shown[0]?.live, false);
  assert.equal(shown[0]?.chat?.seq, 4);
});
