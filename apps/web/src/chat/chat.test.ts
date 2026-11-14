import assert from "node:assert/strict";
import { test } from "node:test";

import type { ChatEntry, ChatEvent, RunEvent, RunState } from "../protocol.ts";
import { type Ask, type Chat, type Item, type Run, answered, commit, fromSnapshot, missing, older, stream, working } from "./chat.ts";

const at = (seq: number, event: ChatEvent): ChatEntry => ({ seq, event });
const said = (seq: number, text: string) => at(seq, { kind: "prompt", request: `q-${seq}`, text });
const ran = (seq: number, run: string, event: RunEvent) => at(seq, { kind: "run", update: { run, event } });
const status = (seq: number, run: string, state: RunState) => at(seq, { kind: "run_status", status: { run, state } });
const text = (seq: number, run: string, words: string) => ran(seq, run, { event: "text", text: words });
const asked = (seq: number, run: string, prompt: number) =>
  ran(seq, run, { event: "permission_asked", run, prompt, tool: "Bash", input: {} });
const finished = (seq: number, run: string) => ran(seq, run, { event: "finished", outcome: { state: "completed" } });
const delta = (words: string): RunEvent => ({ event: "text_delta", text: words });

const runs = (...ids: string[]): Map<string, Run> => new Map(ids.map((id) => [id, { state: "running", agent: undefined }]));

function chat(events: ChatEntry[], seq = events.at(-1)?.seq ?? 0, waiting: number[] = []): Chat {
  return fromSnapshot({ seq, runs: runs("r-1", "r-2"), waiting, events });
}

function commitAll(from: Chat, entries: ChatEntry[]): Chat {
  return entries.reduce((last, entry) => {
    const next = commit(last, entry);
    assert.ok(next !== undefined, `entry ${entry.seq}`);
    return next;
  }, from);
}

// Each item as a short line, to compare whole screens.
function shown(items: readonly Item[]): string[] {
  return items.map((item) => {
    if (item.kind === "prompt") {
      return `> ${item.text}`;
    }
    const { run, transcript, asks, open } = item.turn;
    const rows = transcript.rows.map((row) => (row.kind === "text" ? row.pieces.join("|") : row.kind));
    const answer = (ask: Ask) => (ask.allowed === undefined ? "" : ask.allowed ? " allowed" : " denied");
    const prompts = asks.map((ask) => `ask ${ask.prompt}@${ask.at}${ask.open ? " open" : ""}${answer(ask)}`);
    return `${run}${open ? "" : " done"}: ${[...rows, ...prompts].join(", ")}`;
  });
}

test("a snapshot keeps only the events that run up to its seq without a gap", () => {
  const snapshot = chat([said(2, "old"), said(4, "Hi"), text(5, "r-1", "Hello.")], 5);
  const empty = chat([], 7);

  assert.equal(snapshot.first, 4);
  assert.deepEqual(
    snapshot.entries.map((entry) => entry.seq),
    [4, 5],
  );
  assert.deepEqual(shown(snapshot.items), ["> Hi", "r-1: Hello."]);
  assert.deepEqual([empty.first, empty.seq], [8, 7]);
});

test("entries commit only in order, a repeat changes nothing and a gap gives up", () => {
  const start = chat([said(1, "Hi")]);
  const next = commit(start, text(2, "r-1", "Hello."));

  assert.ok(next !== undefined);
  assert.equal(next.seq, 2);
  assert.equal(commit(next, said(1, "Hi")), next);
  assert.equal(commit(next, said(4, "Later")), undefined);
  assert.deepEqual(shown(next.items), ["> Hi", "r-1: Hello."]);
});

test("each run's turn gathers its events until it finishes, beside another run's", () => {
  const screen = commitAll(chat([]), [
    said(1, "Both of you, look"),
    text(2, "r-1", "Looking."),
    text(3, "r-2", "Me too."),
    finished(4, "r-1"),
    text(5, "r-2", "Found it."),
    said(6, "Again"),
    text(7, "r-1", "Again."),
  ]);

  assert.deepEqual(shown(screen.items), [
    "> Both of you, look",
    "r-1 done: Looking., finished",
    "r-2: Me too., Found it.",
    "> Again",
    "r-1: Again.",
  ]);
});

test("streamed text shows in the run's turn until the committed text replaces it", () => {
  const start = chat([said(1, "Hi")]);
  const streaming = stream(stream(start, "r-1", delta("Hel")), "r-1", delta("lo"));
  const strangers = stream(streaming, "r-9", delta("nope"));
  const notText = stream(streaming, "r-1", { event: "tool_done", call: "c-1", failed: false });
  const committed = commit(streaming, text(2, "r-1", "Hello"));

  assert.deepEqual(shown(streaming.items), ["> Hi", "r-1: Hel|lo"]);
  assert.equal(strangers, streaming);
  assert.equal(notText, streaming);
  assert.ok(committed !== undefined);
  // The same text keeps its pieces, so a selection in them survives.
  assert.deepEqual(shown(committed.items), ["> Hi", "r-1: Hel|lo"]);
  assert.equal(committed.items[1]?.kind === "turn" && committed.items[1].turn.transcript.writing, false);
});

test("text still streaming survives a window of older events", () => {
  const start = chat([said(3, "Hi")], 3);
  const streaming = stream(start, "r-1", delta("Hel"));
  const widened = older(streaming, [said(1, "First"), text(2, "r-2", "Sure.")]);

  assert.deepEqual(shown(widened.items), ["> First", "r-2 done: Sure.", "> Hi", "r-1: Hel"]);
  assert.equal(widened.first, 1);
});

test("a window adds only the events that end just before the first one held", () => {
  const start = chat([said(5, "Hi")], 5);

  const gap = older(start, [said(2, "Far"), said(3, "Near")]);
  const overlap = older(start, [said(3, "a"), said(4, "b"), said(5, "again")]);

  assert.equal(gap, start);
  assert.deepEqual(shown(overlap.items), ["> a", "> b", "> Hi"]);
  assert.equal(overlap.first, 3);
});

test("a prompt is open until it is withdrawn, its run stops waiting or its turn finishes", () => {
  const start = chat([]);
  const waiting = commitAll(start, [text(1, "r-1", "Let me run it."), asked(2, "r-1", 1), asked(3, "r-1", 2)]);
  const withdrawn = commitAll(waiting, [ran(4, "r-1", { event: "permission_withdrawn", run: "r-1", prompt: 1 })]);
  const stillWaiting = commitAll(withdrawn, [status(5, "r-1", "waiting")]);
  const moved = commitAll(stillWaiting, [status(6, "r-1", "running")]);
  const ended = commitAll(waiting, [finished(4, "r-1")]);

  assert.deepEqual(shown(waiting.items), ["r-1: Let me run it., ask 1@1 open, ask 2@1 open"]);
  assert.deepEqual(shown(withdrawn.items), ["r-1: Let me run it., ask 1@1, ask 2@1 open"]);
  assert.deepEqual(shown(stillWaiting.items), shown(withdrawn.items));
  assert.deepEqual(shown(moved.items), ["r-1: Let me run it., ask 1@1, ask 2@1"]);
  assert.deepEqual(shown(ended.items), ["r-1 done: Let me run it., finished, ask 1@1, ask 2@1"]);
  assert.equal(moved.runs.get("r-1")?.state, "running");
});

test("an answer in the log closes its prompt, says how it was answered, and adds nothing to the turn", () => {
  const answer = (seq: number, prompt: number, allow: boolean) =>
    ran(seq, "r-1", { event: "permission_answered", answer: { run: "r-1", prompt, allow } });
  const waiting = commitAll(chat([]), [asked(1, "r-1", 1), asked(2, "r-1", 2), asked(3, "r-1", 3)]);
  const closedHere = answered(waiting, "r-1", 1);
  const told = commitAll(closedHere, [answer(4, 1, true), answer(5, 2, false)]);
  const ended = commitAll(told, [finished(6, "r-1"), answer(7, 3, true)]);

  assert.deepEqual(shown(told.items), ["r-1: ask 1@0 allowed, ask 2@0 denied, ask 3@0 open"]);
  assert.deepEqual(shown(ended.items), ["r-1 done: finished, ask 1@0 allowed, ask 2@0 denied, ask 3@0 allowed"]);
  assert.equal(commitAll(told, [answer(6, 9, true)]).items[0], told.items[0]);
});

test("prompts the snapshot covers are open only if it says they wait", () => {
  const snapshot = chat([asked(1, "r-1", 1), asked(2, "r-1", 2)], 2, [2]);
  const later = commitAll(snapshot, [asked(3, "r-1", 3)]);

  assert.deepEqual(shown(later.items), ["r-1: ask 1@0, ask 2@0 open, ask 3@0 open"]);
});

test("the oldest waiting prompt the chat doesn't hold is the one to fetch", () => {
  const far = chat([said(9, "Hi")], 9, [9, 4, 6]);

  assert.equal(missing(far), 4);
  assert.equal(missing(older(far, [said(4, "a"), said(5, "b"), said(6, "c"), said(7, "d"), said(8, "e")])), undefined);
  assert.equal(missing(chat([said(1, "Hi")], 1, [1])), undefined);
});

test("an answered prompt stops being answerable, and only that one", () => {
  const waiting = chat([asked(1, "r-1", 1), asked(2, "r-1", 2), asked(3, "r-2", 1)], 3, [1, 2, 3]);
  const done = answered(waiting, "r-1", 1);

  assert.deepEqual(shown(done.items), ["r-1: ask 1@0, ask 2@0 open", "r-2: ask 1@0 open"]);
  assert.equal(done.items[1], waiting.items[1]);
  assert.equal(answered(done, "r-1", 9).items[0], done.items[0]);
});

test("a run that stops without finishing ends its turn, so its next one starts after the prompt", () => {
  const screen = commitAll(chat([]), [
    text(1, "r-1", "Working."),
    status(2, "r-1", "idle"),
    said(3, "Go on"),
    text(4, "r-1", "Going on."),
    status(5, "r-1", "running"),
    text(6, "r-1", "Still going."),
  ]);

  assert.deepEqual(shown(screen.items), ["r-1 done: Working.", "> Go on", "r-1: Going on., Still going."]);
});

test("a status keeps the agent the run had when it names none", () => {
  const agent = { kind: "claude" as const, owner: { kind: "shared" as const } };
  const start = fromSnapshot({ seq: 0, runs: new Map([["r-1", { state: "idle", agent }]]), waiting: [], events: [] });
  const next = commitAll(start, [status(1, "r-1", "running"), status(2, "r-3", "starting")]);

  assert.deepEqual(next.runs.get("r-1"), { state: "running", agent });
  assert.deepEqual(next.runs.get("r-3"), { state: "starting", agent: undefined });
});

test("a chat is working while one of its runs starts, runs or waits", () => {
  const states: [RunState, boolean][] = [
    ["starting", true],
    ["running", true],
    ["waiting", true],
    ["idle", false],
    ["sleeping", false],
    ["ended", false],
  ];
  for (const [state, expected] of states) {
    const one = fromSnapshot({ seq: 0, runs: new Map([["r-1", { state, agent: undefined }]]), waiting: [], events: [] });
    assert.equal(working(one), expected, state);
  }
});

test("a prompt loaded with older events still closes when it is withdrawn or its run moves on", () => {
  const start = chat([text(5, "r-1", "Later."), text(6, "r-2", "Other.")], 6, [3, 4]);
  const loaded = older(start, [asked(3, "r-1", 1), asked(4, "r-2", 1)]);
  const withdrawn = commitAll(loaded, [ran(7, "r-1", { event: "permission_withdrawn", run: "r-1", prompt: 1 })]);
  const moved = commitAll(loaded, [status(7, "r-2", "running")]);
  const ended = commitAll(loaded, [finished(7, "r-1")]);

  assert.deepEqual(shown(loaded.items), ["r-1 done: ask 1@0 open", "r-2 done: ask 1@0 open", "r-1: Later.", "r-2: Other."]);
  assert.deepEqual(shown(withdrawn.items).slice(0, 2), ["r-1 done: ask 1@0", "r-2 done: ask 1@0 open"]);
  assert.deepEqual(shown(moved.items).slice(0, 2), ["r-1 done: ask 1@0 open", "r-2 done: ask 1@0"]);
  assert.deepEqual(shown(ended.items).slice(0, 2), ["r-1 done: ask 1@0", "r-2 done: ask 1@0 open"]);
});

test("a prompt a window brings back says how the snapshot's entries answered it", () => {
  const answer = ran(2, "r-1", { event: "permission_answered", answer: { run: "r-1", prompt: 1, allow: false } });
  const start = chat([answer, text(3, "r-1", "Fine.")], 3);
  const loaded = older(start, [asked(1, "r-1", 1)]);

  assert.deepEqual(shown(loaded.items).slice(0, 1), ["r-1 done: ask 1@0 denied"]);
  assert.equal(start.first, 2);
});

test("someone's words from outside Weacon show as a prompt", () => {
  const start = commitAll(chat([]), [at(1, { kind: "outside_prompt", text: "From the terminal." })]);

  assert.deepEqual(shown(start.items), ["> From the terminal."]);
});

test("a prompt withdrawn before the window that brings it back stays closed", () => {
  const start = chat([text(5, "r-1", "Later.")], 5, [3, 4]);
  const withdrawn = commitAll(start, [
    ran(6, "r-1", { event: "permission_withdrawn", run: "r-1", prompt: 1 }),
    status(7, "r-2", "running"),
  ]);
  const loaded = older(withdrawn, [asked(3, "r-1", 1), asked(4, "r-2", 1)]);

  assert.deepEqual(shown(loaded.items).slice(0, 2), ["r-1 done: ask 1@0", "r-2 done: ask 1@0"]);
  assert.deepEqual(shown(older(start, [asked(3, "r-1", 1), asked(4, "r-2", 1)]).items).slice(0, 2), [
    "r-1 done: ask 1@0 open",
    "r-2 done: ask 1@0 open",
  ]);
});

test("a run that stops while it writes leaves its text written", () => {
  const writing = stream(chat([said(1, "Hi")]), "r-1", delta("Hal"));
  const stopped = commitAll(writing, [status(2, "r-1", "idle")]);
  const turn = stopped.items.at(-1);

  assert.equal(turn?.kind === "turn" && turn.turn.transcript.writing, false);
  assert.equal(turn?.kind === "turn" && turn.turn.open, false);
});

test("a run that isn't on a turn streams nothing, even a late piece of its text", () => {
  const start = fromSnapshot({
    seq: 1,
    runs: new Map<string, Run>([
      ["r-1", { state: "idle", agent: undefined }],
      ["r-2", { state: "starting", agent: undefined }],
    ]),
    waiting: [],
    events: [said(1, "Hi")],
  });

  assert.equal(stream(start, "r-1", delta("late")), start);
  assert.deepEqual(shown(stream(start, "r-2", delta("Hel")).items), ["> Hi", "r-2: Hel"]);
});
