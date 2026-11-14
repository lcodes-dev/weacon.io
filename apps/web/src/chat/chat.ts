import type { AgentRef, ChatEntry, RunEvent, RunId, RunState, RunStatus } from "../protocol.ts";
import { EMPTY, type Transcript, apply } from "../transcript/transcript.ts";

/** A permission prompt, shown after `at` rows of its turn. */
export type Ask = { prompt: number; tool: string; at: number; open: boolean };

/**
 * What one run did from its first event after a prompt until it finished; `key` names it for
 * as long as the chat is shown, as older events load before it.
 */
export type Turn = { key: string; run: RunId; transcript: Transcript; asks: readonly Ask[]; open: boolean };

/** What the screen shows, in order: people's prompts and agents' turns. */
export type Item = { kind: "prompt"; seq: number; text: string } | { kind: "turn"; turn: Turn };

/** A run as its chat's log records it. */
export type Run = { state: RunState; agent: AgentRef | undefined };

/**
 * A chat's log from `first` to `seq` with no gaps, its runs, and what it shows, streamed text
 * included. A prompt at or before `base` was waiting at the snapshot only if `waiting` has it.
 */
export type Chat = {
  first: number;
  seq: number;
  base: number;
  waiting: ReadonlySet<number>;
  entries: readonly ChatEntry[];
  runs: ReadonlyMap<RunId, Run>;
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
    items: [],
  };
  return { ...chat, items: fold(chat, entries) };
}

/** The oldest prompt still waiting that the chat doesn't hold yet. */
export function missing(chat: Chat): number | undefined {
  const older = [...chat.waiting].filter((seq) => seq < chat.first);
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
  const runs = new Map(chat.runs);
  const { event } = entry;
  if (event.kind === "run_status") {
    runs.set(event.status.run, { state: event.status.state, agent: agentOf(event.status, runs) });
  }
  step(items, entry, true);
  return { ...chat, seq: entry.seq, entries: [...chat.entries, entry], runs, items };
}

/** `chat` with the text a run streams, when the run is one of the chat's. */
export function stream(chat: Chat, run: RunId, event: RunEvent): Chat {
  if (event.event !== "text_delta" || !chat.runs.has(run)) {
    return chat;
  }
  const items = [...chat.items];
  addTo(items, run, turnKey(chat.seq, run), (turn) => ({ ...turn, transcript: apply(turn.transcript, [event]) }));
  return { ...chat, items };
}

/**
 * `chat` with the older `events` that end just before its first, as a window answers. The turns
 * shown already stay as they are, so a selection in them survives; a run's turn the window cuts
 * in two shows as two.
 */
export function older(chat: Chat, events: readonly ChatEntry[]): Chat {
  const before = contiguous(
    events.filter((entry) => entry.seq < chat.first),
    chat.first - 1,
  );
  if (before.length === 0) {
    return chat;
  }
  const earlier = fold(chat, before).map((item) => (item.kind === "turn" ? { ...item, turn: { ...item.turn, open: false } } : item));
  // What the log said after the snapshot also closes the prompts this window brings back.
  for (const entry of chat.entries) {
    if (entry.seq > chat.base) {
      closeBy(earlier, entry);
    }
  }
  return { ...chat, first: before[0]?.seq ?? chat.first, entries: [...before, ...chat.entries], items: [...earlier, ...chat.items] };
}

/** `chat` with the prompt `prompt` of `run` no longer answerable, as once its answer is taken. */
export function answered(chat: Chat, run: RunId, prompt: number): Chat {
  const items = [...chat.items];
  closeAsks(items, run, (ask) => ask.prompt === prompt);
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

// The items `entries` make.
function fold(chat: Chat, entries: readonly ChatEntry[]): Item[] {
  const items: Item[] = [];
  for (const entry of entries) {
    step(items, entry, entry.seq > chat.base || chat.waiting.has(entry.seq));
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
  closeBy(items, entry);
  // A run that stops without a finished event, as when its core crashed, ends its turn too.
  if (event.kind === "run_status" && !["starting", "running", "waiting"].includes(event.status.state)) {
    update(items, event.status.run, (turn) => ({ ...turn, open: false, transcript: { ...turn.transcript, writing: false } }));
  }
  if (event.kind !== "run" || event.update.event.event === "permission_withdrawn") {
    return;
  }
  const { run, event: done } = event.update;
  addTo(items, run, turnKey(entry.seq - 1, run), (turn) => {
    if (done.event === "permission_asked") {
      const ask = { prompt: done.prompt, tool: done.tool, at: turn.transcript.rows.length, open: waiting };
      return { ...turn, asks: [...turn.asks, ask] };
    }
    const transcript = apply(turn.transcript, [done]);
    if (done.event !== "finished") {
      return { ...turn, transcript };
    }
    return { ...turn, transcript, open: false };
  });
}

// Changes `run`'s open turn, starting one called `key` at the end when it has none.
function addTo(items: Item[], run: RunId, key: string, change: (turn: Turn) => Turn): void {
  if (!update(items, run, change)) {
    items.push({ kind: "turn", turn: change({ key, run, transcript: EMPTY, asks: [], open: true }) });
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

// Closes the prompts `entry` says no longer wait: a withdrawn one, or all of a run that finished
// or stopped waiting.
function closeBy(items: Item[], entry: ChatEntry): void {
  const { event } = entry;
  if (event.kind === "run_status" && event.status.state !== "waiting") {
    closeAsks(items, event.status.run, () => true);
  } else if (event.kind === "run" && event.update.event.event === "permission_withdrawn") {
    const { prompt } = event.update.event;
    closeAsks(items, event.update.run, (ask) => ask.prompt === prompt);
  } else if (event.kind === "run" && event.update.event.event === "finished") {
    closeAsks(items, event.update.run, () => true);
  }
}

// Closes the open prompts `which` picks in every turn of `run`, such as one cut in two by a window.
function closeAsks(items: Item[], run: RunId, which: (ask: Ask) => boolean): void {
  for (const [at, item] of items.entries()) {
    if (item.kind === "turn" && item.turn.run === run) {
      const turn = closing(item.turn, which);
      if (turn !== item.turn) {
        items[at] = { kind: "turn", turn };
      }
    }
  }
}

// `turn` with the open prompts `which` picks closed; the same turn when it picks none.
function closing(turn: Turn, which: (ask: Ask) => boolean): Turn {
  if (!turn.asks.some((ask) => ask.open && which(ask))) {
    return turn;
  }
  return { ...turn, asks: turn.asks.map((ask) => (ask.open && which(ask) ? { ...ask, open: false } : ask)) };
}

// The same for a turn whether its first event was streamed or committed after `seq`.
function turnKey(seq: number, run: RunId): string {
  return `t${seq}-${run}`;
}


// A status that names no agent keeps the one the run had.
function agentOf(status: RunStatus, runs: ReadonlyMap<RunId, Run>): AgentRef | undefined {
  return status.agent ?? runs.get(status.run)?.agent;
}
