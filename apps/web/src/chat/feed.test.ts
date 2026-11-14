import assert from "node:assert/strict";
import { test } from "node:test";

import type { Envelope } from "../protocol.ts";
import { ChatFeed, nextFrame } from "./feed.ts";
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
  feed.push(answer(1, 4));
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

  flushes[0]?.();
  assert.equal(shown.length, 1);
  feed.push(delta("!"));
  assert.equal(flushes.length, 2);
});

test("a taken answer stops its prompt being answerable", () => {
  const { feed, shown, flushes } = wired();
  feed.answered("r-1", 1);
  assert.equal(flushes.length, 0);
  feed.open();
  feed.push({
    id: 1,
    type: "chat_answer",
    workspace: "w-1",
    chat: "c-1",
    answer: {
      kind: "snapshot",
      seq: 1,
      runs: [{ run: "r-1", state: "waiting" }],
      waiting: [1],
      events: [{ seq: 1, event: { kind: "run", update: { run: "r-1", event: { event: "permission_asked", run: "r-1", prompt: 1, tool: "Bash", input: null } } } }],
    },
  });
  feed.answered("r-1", 1);
  flushes[0]?.();
  const turn = shown[0]?.chat?.items[0];

  assert.equal(turn?.kind === "turn" && turn.turn.asks[0]?.open, false);
});

test("what changes nothing schedules nothing, and other chats' messages aren't taken", () => {
  const { feed, sent, flushes } = wired();
  feed.push({ ...answer(1, 4), chat: "c-2" });
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

test("a batch goes out on the next frame, or on the timer once the tab is hidden", () => {
  let hidden = false;
  const frames = new Map<number, () => void>();
  const timers: (() => void)[] = [];
  const schedule = nextFrame(
    () => hidden,
    (run) => frames.set(frames.size + 1, run).size,
    (frame) => frames.delete(frame),
    (run) => timers.push(run),
  );
  const sent: string[] = [];

  schedule(() => sent.push("shown"));
  timers.shift()?.();
  assert.equal(sent.length, 0);
  frames.get(1)?.();
  assert.deepEqual(sent, ["shown"]);

  schedule(() => sent.push("hidden"));
  hidden = true;
  timers.shift()?.();
  assert.deepEqual(sent, ["shown", "hidden"]);
  assert.deepEqual([...frames.keys()], [1]);
});
