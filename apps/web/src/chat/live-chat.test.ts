import assert from "node:assert/strict";
import { test } from "node:test";

import type { Envelope } from "../protocol.ts";
import { LiveChat, nextFrame } from "./live-chat.ts";
import type { Sync } from "./sync.ts";

function wired() {
  const sent: Envelope[] = [];
  const shown: Sync[] = [];
  const flushes: (() => void)[] = [];
  let next = 1;
  const live = new LiveChat(
    { workspace: "w-1", chat: "c-1" },
    {
      id: () => next++,
      send: (envelope) => sent.push(envelope),
      schedule: (flush) => flushes.push(flush),
      show: (sync) => shown.push(sync),
    },
  );
  return { live, sent, shown, flushes };
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
  const { live, sent, shown, flushes } = wired();
  live.open();
  live.push(answer(1, 4));
  live.push(delta("He"));
  live.push(delta("llo"));

  assert.deepEqual(
    sent.map((envelope) => envelope.type === "chat_query" && envelope.query.kind),
    ["snapshot", "subscribe"],
  );
  assert.equal(flushes.length, 1);
  assert.equal(shown.length, 0);
  flushes[0]?.();
  assert.equal(shown.length, 1);
  const turn = shown[0]?.chat?.items.at(-1);
  assert.deepEqual(turn?.kind === "turn" && turn.turn.transcript.rows, [{ kind: "text", pieces: ["He", "llo"] }]);

  flushes[0]?.();
  assert.equal(shown.length, 1);
  live.push(delta("!"));
  assert.equal(flushes.length, 2);
});

test("a taken answer stops its prompt being answerable", () => {
  const { live, shown, flushes } = wired();
  live.answered("r-1", 1);
  assert.equal(flushes.length, 0);
  live.open();
  live.push({
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
  live.answered("r-1", 1);
  flushes[0]?.();
  const turn = shown[0]?.chat?.items[0];

  assert.equal(turn?.kind === "turn" && turn.turn.asks[0]?.state, "closed");
});

test("what changes nothing schedules nothing, and other chats' messages aren't taken", () => {
  const { live, sent, flushes } = wired();
  live.push({ ...answer(1, 4), chat: "c-2" });
  live.more();
  live.end();
  assert.deepEqual([sent, flushes], [[], []]);
});

test("the end of the connection keeps the chat but drops what was asked", () => {
  const { live, shown, flushes } = wired();
  live.open();
  live.push(answer(1, 4));
  live.end();
  live.push(answer(1, 9));
  flushes[0]?.();

  assert.equal(shown[0]?.state.kind, "idle");
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

test("once closed, a live chat sends and shows nothing more, a flush to come included", () => {
  const { live, sent, shown, flushes } = wired();
  live.open();
  live.close();
  live.push(answer(1, 4));
  live.more();
  flushes.forEach((flush) => flush());

  assert.equal(sent.length, 1);
  assert.deepEqual(shown, []);
});
