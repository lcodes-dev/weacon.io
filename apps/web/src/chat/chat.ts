import type { AgentRef, ChatEntry, MemberId, PickNote, RunEvent, RunId, RunState, RunStatus, Wants } from "../protocol.ts";
import { EMPTY_TRANSCRIPT, type Transcript, apply } from "../transcript/transcript.ts";

/** A permission prompt and what its tool wants, shown after `at` rows of its turn, and how the log says it ended. */
export type Ask = { prompt: number; tool: string; wants: Wants | undefined; at: number; state: "open" | "closed" | "allowed" | "denied" };

/**
 * What one run did from its first event after a prompt until it finished; `key` names it for
 * as long as the chat is shown, as older events load before it. `texts` has the row each of its
 * committed messages shows in, by the message's seq.
 */
export type Turn = { key: string; run: RunId; transcript: Transcript; asks: readonly Ask[]; open: boolean; texts: ReadonlyMap<number, number> };

/**
 * What the screen shows, in order: what people wrote, to the chat or to their own agent, by
 * `author` when the log says who, and agents' turns.
 */
export type Item = { kind: "prompt"; seq: number; text: string; author: MemberId | undefined; pick?: PickNote } | { kind: "turn"; turn: Turn };

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
    runs: logged(snapshot.runs, entries),
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

/**
 * `chat` with the text a run streams live, when the run is one of the chat's on a turn and the
 * chat holds no event after `after`.
 */
export function stream(chat: Chat, run: RunId, event: RunEvent, after?: number): Chat {
  const state = chat.runs.get(run)?.state;
  // Text from before the chat's latest event is in that event already.
  const late = after !== undefined && after < chat.seq;
  if (event.event !== "text_delta" || state === undefined || !ON_TURN.includes(state) || late) {
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
  // What the log said after the snapshot also closes the prompts this window brings back; the
  // snapshot's own entries only say how they were answered, as it says which still wait.
  for (const entry of chat.entries) {
    if (entry.seq > chat.base) {
      closeBy(earlier, entry);
    } else {
      showAnswer(earlier, entry);
    }
  }
  return {
    ...chat,
    first: before[0]?.seq ?? chat.first,
    entries: [...before, ...chat.entries],
    runs: logged(chat.runs, before),
    items: [...earlier, ...chat.items],
  };
}

/** `chat` with the prompt `prompt` of `run` no longer answerable, as once its answer is taken. */
export function answered(chat: Chat, run: RunId, prompt: number): Chat {
  const items = [...chat.items];
  closeAsks(items, run, (ask) => ask.prompt === prompt);
  return { ...chat, items };
}

/**
 * Whether a run of `me`'s own agent is on a turn in the chat, so that they can stop it; a run whose
 * owner the log doesn't name counts as theirs.
 */
export function working(chat: Chat, me: MemberId | undefined): boolean {
  const theirs = ({ agent }: Run) => agent?.owner.kind !== "member" || agent.owner.member === me;
  return [...chat.runs.values()].some((run) => ON_TURN.includes(run.state) && theirs(run));
}

// The states of a run on a turn, which streams text and can be stopped.
const ON_TURN: readonly RunState[] = ["starting", "running", "waiting"];

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
  if (event.kind === "message" || event.kind === "prompt" || event.kind === "outside_prompt") {
    const pick = event.kind === "prompt" ? event.pick : undefined;
    const author = event.kind === "outside_prompt" ? undefined : event.author;
    items.push({ kind: "prompt", seq: entry.seq, text: event.text, author, ...(pick === undefined ? {} : { pick }) });
    return;
  }
  closeBy(items, entry);
  // A run that stops without a finished event, as when its core crashed, ends its turn too.
  if (event.kind === "run_status" && !["starting", "running", "waiting"].includes(event.status.state)) {
    update(items, event.status.run, (turn) => ({ ...turn, open: false, transcript: { ...turn.transcript, writing: false } }));
  }
  if (event.kind !== "run" || ["permission_withdrawn", "permission_answered"].includes(event.update.event.event)) {
    return;
  }
  const { run, event: done } = event.update;
  addTo(items, run, turnKey(entry.seq - 1, run), (turn) => {
    if (done.event === "permission_asked") {
      const ask: Ask = { prompt: done.prompt, tool: done.tool, wants: done.wants, at: turn.transcript.rows.length, state: waiting ? "open" : "closed" };
      return { ...turn, asks: [...turn.asks, ask] };
    }
    const transcript = apply(turn.transcript, [done]);
    const texts = done.event === "text" ? new Map(turn.texts).set(entry.seq, transcript.rows.length - 1) : turn.texts;
    return { ...turn, transcript, texts, open: turn.open && done.event !== "finished" };
  });
}

// Changes `run`'s open turn, starting one called `key` at the end when it has none.
function addTo(items: Item[], run: RunId, key: string, change: (turn: Turn) => Turn): void {
  if (!update(items, run, change)) {
    items.push({ kind: "turn", turn: change({ key, run, transcript: EMPTY_TRANSCRIPT, asks: [], open: true, texts: new Map() }) });
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
  } else {
    showAnswer(items, entry);
  }
}

// Closes the prompt `entry` answers, if it is an answer, saying how.
function showAnswer(items: Item[], { event }: ChatEntry): void {
  if (event.kind === "run" && event.update.event.event === "permission_answered") {
    const { prompt, allow } = event.update.event.answer;
    changeAsks(items, event.update.run, (ask) => (ask.prompt === prompt ? { ...ask, state: allow ? "allowed" : "denied" } : ask));
  }
}

// Closes the open prompts `which` picks in every turn of `run`, such as one cut in two by a window.
function closeAsks(items: Item[], run: RunId, which: (ask: Ask) => boolean): void {
  changeAsks(items, run, (ask) => (ask.state === "open" && which(ask) ? { ...ask, state: "closed" } : ask));
}

// Changes the prompts of every turn of `run`; a turn whose prompts `change` all keeps stays the same.
function changeAsks(items: Item[], run: RunId, change: (ask: Ask) => Ask): void {
  for (const [at, item] of items.entries()) {
    if (item.kind === "turn" && item.turn.run === run) {
      const asks = item.turn.asks.map(change);
      if (asks.some((ask, index) => ask !== item.turn.asks[index])) {
        items[at] = { kind: "turn", turn: { ...item.turn, asks } };
      }
    }
  }
}

// The same for a turn whether its first event was streamed or committed after `seq`.
function turnKey(seq: number, run: RunId): string {
  return `t${seq}-${run}`;
}

/**
 * `runs` with the runs whose statuses `entries` log, so an earlier run's turns name its agent. A
 * run `runs` already holds keeps its state; one only the log knows is over, since `runs` has every run still on.
 */
function logged(runs: ReadonlyMap<RunId, Run>, entries: readonly ChatEntry[]): ReadonlyMap<RunId, Run> {
  const next = new Map(runs);
  for (const { event } of entries) {
    if (event.kind === "run_status") {
      const known = runs.get(event.status.run);
      next.set(event.status.run, { state: known?.state ?? "ended", agent: known?.agent ?? agentOf(event.status, next) });
    }
  }
  return next;
}

// A status that names no agent keeps the one the run had.
function agentOf(status: RunStatus, runs: ReadonlyMap<RunId, Run>): AgentRef | undefined {
  return status.agent ?? runs.get(status.run)?.agent;
}
