import type { ChatEntry, RunEvent, RunId, RunState } from "../protocol.ts";
import { EMPTY, type Transcript, apply } from "../transcript.ts";
import type { RunAgent } from "./label.ts";

/** A permission prompt, shown after `at` rows of its turn; `seq` is its place in the log. */
export type Ask = { seq: number; prompt: number; tool: string; at: number; open: boolean };

/** What one run did from its first event after a prompt until it finished. */
export type Turn = { run: RunId; transcript: Transcript; asks: readonly Ask[]; open: boolean };

/** What the screen shows, in order: people's prompts and agents' turns. */
export type Item = { kind: "prompt"; seq: number; text: string } | { kind: "turn"; turn: Turn };

/** A run as its chat's log records it. */
export type Run = { state: RunState; agent: RunAgent | undefined };

/**
 * A chat's log from `first` to `seq` with nothing missing, its runs, and the text its runs are
 * writing that isn't committed yet. Prompts at or before `base` were waiting as of the snapshot
 * only if `waiting` holds them.
 */
export type Chat = {
  first: number;
  seq: number;
  base: number;
  waiting: ReadonlySet<number>;
  entries: readonly ChatEntry[];
  runs: ReadonlyMap<RunId, Run>;
  live: ReadonlyMap<RunId, readonly RunEvent[]>;
  items: readonly Item[];
};

/** The parts of a snapshot this file reads. */
export type Snapshot = {
  seq: number;
  runs: ReadonlyMap<RunId, Run>;
  waiting: readonly number[];
  events: readonly ChatEntry[];
};

/** The chat as `snapshot` has it: its latest events with nothing missing before `seq`. */
export function fromSnapshot(snapshot: Snapshot): Chat {
  const entries = contiguous(snapshot.events, snapshot.seq);
  const chat: Chat = {
    first: entries[0]?.seq ?? snapshot.seq + 1,
    seq: snapshot.seq,
    base: snapshot.seq,
    waiting: new Set(snapshot.waiting),
    entries,
    runs: snapshot.runs,
    live: new Map(),
    items: [],
  };
  return { ...chat, items: fold(chat, entries) };
}

/** The oldest prompt still waiting that the chat doesn't hold yet. */
export function missing(chat: Chat): number | undefined {
  const older = [...chat.waiting].filter((seq) => seq >= 1 && seq < chat.first);
  return older.length === 0 ? undefined : Math.min(...older);
}

/**
 * `chat` with `entry` committed, or `undefined` when it isn't the next one, as after missed
 * events. A repeat of one it holds changes nothing.
 */
export function commit(chat: Chat, entry: ChatEntry): Chat | undefined {
  if (entry.seq <= chat.seq) {
    return chat;
  }
  if (entry.seq !== chat.seq + 1) {
    return undefined;
  }
  const items = [...chat.items];
  const live = new Map(chat.live);
  const runs = new Map(chat.runs);
  const { event } = entry;
  if (event.kind === "run_status") {
    runs.set(event.status.run, { state: event.status.state, agent: agentOf(event.status, runs) });
  } else if (event.kind === "run" && ends(event.update.event)) {
    live.delete(event.update.run);
  }
  step(items, entry, true);
  return { ...chat, seq: entry.seq, entries: [...chat.entries, entry], runs, live, items };
}

/** `chat` with the text a run streams, when the run is one of the chat's. */
export function stream(chat: Chat, run: RunId, event: RunEvent): Chat {
  if (event.event !== "text_delta" || !chat.runs.has(run)) {
    return chat;
  }
  const items = [...chat.items];
  addTo(items, run, (turn) => ({ ...turn, transcript: apply(turn.transcript, [event]) }));
  const live = new Map(chat.live).set(run, [...(chat.live.get(run) ?? []), event]);
  return { ...chat, live, items };
}

/** `chat` with the older `events` that end just before its first, as a window answers. */
export function older(chat: Chat, events: readonly ChatEntry[]): Chat {
  const before = contiguous(
    events.filter((entry) => entry.seq < chat.first),
    chat.first - 1,
  );
  if (before.length === 0) {
    return chat;
  }
  const entries = [...before, ...chat.entries];
  const next = { ...chat, first: before[0]?.seq ?? chat.first, entries };
  return { ...next, items: fold(next, entries) };
}

/** `chat` with the prompt `prompt` of `run` no longer answerable, as once its answer is taken. */
export function answered(chat: Chat, run: RunId, prompt: number): Chat {
  const items = chat.items.map((item) =>
    item.kind === "turn" && item.turn.run === run && item.turn.asks.some((ask) => ask.prompt === prompt && ask.open)
      ? { ...item, turn: { ...item.turn, asks: item.turn.asks.map((ask) => (ask.prompt === prompt ? { ...ask, open: false } : ask)) } }
      : item,
  );
  return { ...chat, items };
}

/** Whether a run of the chat is on a turn, so that it can be stopped. */
export function working(chat: Chat): boolean {
  return [...chat.runs.values()].some((run) => ["starting", "running", "waiting"].includes(run.state));
}

// The entries of `events` that run without a gap up to `last`, the newest first found.
function contiguous(events: readonly ChatEntry[], last: number): ChatEntry[] {
  const sorted = [...events].sort((a, b) => b.seq - a.seq);
  const kept: ChatEntry[] = [];
  for (const entry of sorted) {
    const want = (kept.at(-1)?.seq ?? last + 1) - 1;
    if (entry.seq === want) {
      kept.push(entry);
    } else if (entry.seq < want) {
      break;
    }
  }
  return kept.reverse();
}

// The items `entries` make, with the text still streaming after them.
function fold(chat: Chat, entries: readonly ChatEntry[]): Item[] {
  const items: Item[] = [];
  for (const entry of entries) {
    step(items, entry, entry.seq > chat.base || chat.waiting.has(entry.seq));
  }
  for (const [run, events] of chat.live) {
    addTo(items, run, (turn) => ({ ...turn, transcript: apply(turn.transcript, events) }));
  }
  return items;
}

// Applies one committed entry to `items`; `waiting` says whether a prompt it asks is open.
function step(items: Item[], entry: ChatEntry, waiting: boolean): void {
  const { event } = entry;
  if (event.kind === "prompt") {
    items.push({ kind: "prompt", seq: entry.seq, text: event.text });
    return;
  }
  if (event.kind === "run_status" && event.status.state !== "waiting") {
    closeAsks(items, event.status.run);
  }
  if (event.kind !== "run") {
    return;
  }
  const { run, event: done } = event.update;
  if (done.event === "permission_withdrawn") {
    update(items, run, (turn) => ({ ...turn, asks: turn.asks.map((ask) => (ask.prompt === done.prompt ? { ...ask, open: false } : ask)) }));
    return;
  }
  addTo(items, run, (turn) => {
    if (done.event === "permission_asked") {
      const ask = { seq: entry.seq, prompt: done.prompt, tool: done.tool, at: turn.transcript.rows.length, open: waiting };
      return { ...turn, asks: [...turn.asks, ask] };
    }
    const transcript = apply(turn.transcript, [done]);
    if (done.event !== "finished") {
      return { ...turn, transcript };
    }
    return { ...turn, transcript, open: false, asks: turn.asks.map((ask) => ({ ...ask, open: false })) };
  });
}

// Changes `run`'s open turn, starting one at the end when it has none.
function addTo(items: Item[], run: RunId, change: (turn: Turn) => Turn): void {
  if (!update(items, run, change)) {
    items.push({ kind: "turn", turn: change({ run, transcript: EMPTY, asks: [], open: true }) });
  }
}

// Changes `run`'s open turn, if it has one; whether it had.
function update(items: Item[], run: RunId, change: (turn: Turn) => Turn): boolean {
  const at = items.findLastIndex((item) => item.kind === "turn" && item.turn.run === run && item.turn.open);
  const item = items[at];
  if (item?.kind !== "turn") {
    return false;
  }
  items[at] = { kind: "turn", turn: change(item.turn) };
  return true;
}

function closeAsks(items: Item[], run: RunId): void {
  update(items, run, (turn) =>
    turn.asks.some((ask) => ask.open) ? { ...turn, asks: turn.asks.map((ask) => ({ ...ask, open: false })) } : turn,
  );
}

function ends(event: RunEvent): boolean {
  return event.event === "text" || event.event === "finished";
}

// A status that names no agent keeps the one the run had.
function agentOf(status: { run: RunId; agent?: RunAgent }, runs: ReadonlyMap<RunId, Run>): RunAgent | undefined {
  return status.agent ?? runs.get(status.run)?.agent;
}
