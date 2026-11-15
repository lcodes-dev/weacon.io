import type { ChatEntry, PickNote, RunId, RunState } from "./protocol.ts";

/**
 * A chat's log from `first` to `seq` with no gaps, and its runs' states. A permission prompt at or
 * before `base`, the snapshot's seq, was waiting then only if `waiting` has its place.
 */
export type Chat = {
  first: number;
  seq: number;
  base: number;
  waiting: ReadonlySet<number>;
  entries: readonly ChatEntry[];
  runs: ReadonlyMap<RunId, RunState>;
};

/** What a snapshot answers, as this file reads it. */
export type Snapshot = {
  seq: number;
  runs: ReadonlyArray<{ run: RunId; state: RunState }>;
  waiting: readonly number[];
  events: readonly ChatEntry[];
};

/** One line of the transcript, under a key that names it for as long as the chat is shown. */
export type Line =
  | { key: string; kind: "prompt"; text: string; pick?: PickNote | undefined }
  | { key: string; kind: "text"; text: string }
  | { key: string; kind: "tool"; tool: string; state: "running" | "done" | "failed" }
  | { key: string; kind: "ask"; run: RunId; prompt: number; tool: string; state: AskState }
  | { key: string; kind: "ended"; code: string };

export type AskState = "open" | "allowed" | "denied" | "closed";

// The states of a run on a turn, which can be stopped.
const ON_TURN: readonly RunState[] = ["starting", "running", "waiting"];

/** The chat as `snapshot` has it: its latest events with nothing missing before `seq`. */
export function fromSnapshot(snapshot: Snapshot): Chat {
  const entries = contiguous(snapshot.events, snapshot.seq);
  return {
    first: entries[0]?.seq ?? snapshot.seq + 1,
    seq: snapshot.seq,
    base: snapshot.seq,
    waiting: new Set(snapshot.waiting),
    entries,
    runs: new Map(snapshot.runs.map(({ run, state }) => [run, state])),
  };
}

/** `chat` with `entry` committed; `undefined` when it isn't the next one. A repeat changes nothing. */
export function commit(chat: Chat, entry: ChatEntry): Chat | undefined {
  if (entry.seq <= chat.seq) {
    return chat;
  }
  if (entry.seq !== chat.seq + 1) {
    return undefined;
  }
  const { event } = entry;
  const runs = event.kind === "run_status" ? new Map(chat.runs).set(event.status.run, event.status.state) : chat.runs;
  return { ...chat, seq: entry.seq, entries: [...chat.entries, entry], runs };
}

/** `chat` with the older `events` that end just before its first, as a window answers. */
export function older(chat: Chat, events: readonly ChatEntry[]): Chat {
  const before = contiguous(
    events.filter((entry) => entry.seq < chat.first),
    chat.first - 1,
  );
  const first = before[0]?.seq ?? chat.first;
  return before.length === 0 ? chat : { ...chat, first, entries: [...before, ...chat.entries] };
}

/** The oldest waiting prompt the chat doesn't hold yet. */
export function missing(chat: Chat): number | undefined {
  const older = [...chat.waiting].filter((seq) => seq < chat.first);
  return older.length === 0 ? undefined : Math.min(...older);
}

/** Whether a run of the chat is on a turn, so that it can be stopped. */
export function working(chat: Chat): boolean {
  return [...chat.runs.values()].some((state) => ON_TURN.includes(state));
}

/** The lines `chat` shows. */
export function lines(chat: Chat): Line[] {
  const shown: Line[] = [];
  for (const entry of chat.entries) {
    step(shown, entry, entry.seq > chat.base || chat.waiting.has(entry.seq));
  }
  return shown;
}

// Applies one entry; `waiting` says whether a prompt it asks is open.
function step(shown: Line[], { seq, event }: ChatEntry, waiting: boolean): void {
  switch (event.kind) {
    case "prompt":
      shown.push({ key: `p${seq}`, kind: "prompt", text: event.text, pick: event.pick });
      return;
    case "outside_prompt":
      shown.push({ key: `p${seq}`, kind: "prompt", text: event.text });
      return;
    case "receipt":
      if (event.receipt.state === "failed") {
        shown.push({ key: `e${seq}`, kind: "ended", code: event.receipt.code });
      }
      return;
    case "run_status":
      if (event.status.state !== "waiting") {
        closeAsks(shown, event.status.run, () => true);
      }
      return;
    case "run":
      return run(shown, seq, event.update.run, event.update.event, waiting);
  }
}

type RunEvent = Extract<ChatEntry["event"], { kind: "run" }>["update"]["event"];

function run(shown: Line[], seq: number, id: RunId, event: RunEvent, waiting: boolean): void {
  switch (event.event) {
    case "text":
      shown.push({ key: `x${seq}`, kind: "text", text: event.text });
      return;
    case "tool_call":
      shown.push({ key: `c${event.call}`, kind: "tool", tool: event.tool, state: "running" });
      return;
    case "tool_done": {
      const state = event.failed ? "failed" : "done";
      change(shown, (line) => (line.kind === "tool" && line.key === `c${event.call}` ? { ...line, state } : line));
      return;
    }
    case "permission_asked":
      shown.push({ key: `a${seq}`, kind: "ask", run: id, prompt: event.prompt, tool: event.tool, state: waiting ? "open" : "closed" });
      return;
    case "permission_withdrawn":
      return closeAsks(shown, id, (prompt) => prompt === event.prompt);
    case "permission_answered": {
      const { prompt, allow } = event.answer;
      const state = allow ? "allowed" : "denied";
      change(shown, (line) => (line.kind === "ask" && line.run === id && line.prompt === prompt ? { ...line, state } : line));
      return;
    }
    case "finished":
      closeAsks(shown, id, () => true);
      if (event.outcome.state === "failed") {
        shown.push({ key: `e${seq}`, kind: "ended", code: event.outcome.code });
      } else if (event.outcome.state === "interrupted") {
        shown.push({ key: `e${seq}`, kind: "ended", code: "run.interrupted" });
      }
      return;
    case "started":
    case "text_delta":
      return;
  }
}

function closeAsks(shown: Line[], id: RunId, which: (prompt: number) => boolean): void {
  change(shown, (line) => (line.kind === "ask" && line.run === id && line.state === "open" && which(line.prompt) ? { ...line, state: "closed" } : line));
}

function change(shown: Line[], to: (line: Line) => Line): void {
  shown.forEach((line, at) => (shown[at] = to(line)));
}

// The entries of `events` that run without a gap up to `last`.
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
