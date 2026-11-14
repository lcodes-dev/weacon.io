import type { Place } from "../place.ts";
import type { ChatEntry, Envelope, RunEvent, RunId } from "../protocol.ts";
import { isRunEvent } from "../transcript.ts";
import type { Run, Snapshot } from "./chat.ts";
import type { Owner, RunAgent } from "./label.ts";

/** What the server said about the chat, checked where it arrives. */
export type Heard =
  | { kind: "entry"; entry: ChatEntry }
  | { kind: "lost" }
  | { kind: "snapshot"; id: number; snapshot: Snapshot }
  | { kind: "window"; id: number; events: ChatEntry[] }
  | { kind: "run"; run: RunId; event: RunEvent };

/** What `envelope` says about the chat at `place`, or `undefined` when it isn't about it or is malformed. */
export function about(envelope: Envelope, place: Place): Heard | undefined {
  if (envelope.type === "run") {
    return typeof envelope.run === "string" && isEvent(envelope.event)
      ? { kind: "run", run: envelope.run, event: envelope.event }
      : undefined;
  }
  if ((envelope.type !== "chat" && envelope.type !== "chat_answer") || envelope.workspace !== place.workspace || envelope.chat !== place.chat) {
    return undefined;
  }
  if (envelope.type === "chat") {
    const entry = entryOf(envelope.entry);
    return entry === undefined ? undefined : { kind: "entry", entry };
  }
  const answer = record(envelope.answer);
  const events = entries(answer?.["events"]);
  switch (answer?.["kind"]) {
    case "events_lost":
      return { kind: "lost" };
    case "window":
      return events === undefined ? undefined : { kind: "window", id: envelope.id, events };
    case "snapshot": {
      const { seq, runs, waiting } = answer;
      if (!isSeq(seq, 0) || !Array.isArray(runs) || !Array.isArray(waiting) || events === undefined) {
        return undefined;
      }
      const snapshot = { seq, runs: runsOf(runs), waiting: waiting.filter((at) => isSeq(at, 1)), events };
      return { kind: "snapshot", id: envelope.id, snapshot };
    }
    default:
      return undefined;
  }
}

function entries(list: unknown): ChatEntry[] | undefined {
  if (!Array.isArray(list)) {
    return undefined;
  }
  const kept = list.map(entryOf);
  return kept.every((entry) => entry !== undefined) ? kept : undefined;
}

// An entry whose event this app reads has every field it reads; one of a newer kind keeps its place.
function entryOf(value: unknown): ChatEntry | undefined {
  const entry = record(value);
  const event = record(entry?.["event"]);
  if (entry === undefined || !isSeq(entry["seq"], 1) || event === undefined || typeof event["kind"] !== "string") {
    return undefined;
  }
  const fits = (() => {
    switch (event["kind"]) {
      case "prompt":
        return typeof event["request"] === "string" && typeof event["text"] === "string";
      case "receipt":
        return typeof record(event["receipt"])?.["state"] === "string";
      case "run": {
        const update = record(event["update"]);
        return typeof update?.["run"] === "string" && isEvent(update["event"]);
      }
      case "run_status":
        return statusOf(event["status"]) !== undefined;
      default:
        return true;
    }
  })();
  if (!fits) {
    return undefined;
  }
  const status = event["kind"] === "run_status" ? statusOf(event["status"]) : undefined;
  return (status === undefined ? value : { seq: entry["seq"], event: { kind: "run_status", status } }) as ChatEntry;
}

type Status = { run: RunId; state: Run["state"]; agent?: RunAgent };

function statusOf(value: unknown): Status | undefined {
  const status = record(value);
  if (typeof status?.["run"] !== "string" || typeof status["state"] !== "string") {
    return undefined;
  }
  const agent = agentOf(status["agent"]);
  const state = status["state"] as Run["state"];
  return agent === undefined ? { run: status["run"], state } : { run: status["run"], state, agent };
}

function runsOf(list: unknown[]): Map<RunId, Run> {
  const runs = new Map<RunId, Run>();
  for (const status of list.map(statusOf)) {
    if (status !== undefined) {
      runs.set(status.run, { state: status.state, agent: status.agent });
    }
  }
  return runs;
}

// The parts of an agent that are well-formed; an owner of a newer kind is left out.
function agentOf(value: unknown): RunAgent | undefined {
  const agent = record(value);
  if (agent === undefined) {
    return undefined;
  }
  const owner = record(agent["owner"]);
  const kept: RunAgent = {};
  if (typeof agent["kind"] === "string") {
    kept.kind = agent["kind"];
  }
  if (typeof agent["machine"] === "string") {
    kept.machine = agent["machine"];
  }
  const member = owner?.["member"];
  const known: Owner | undefined =
    owner?.["kind"] === "shared" ? { kind: "shared" } : owner?.["kind"] === "member" && typeof member === "string" ? { kind: "member", member } : undefined;
  if (known !== undefined) {
    kept.owner = known;
  }
  return kept;
}

function record(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>) : undefined;
}

function isSeq(value: unknown, least: number): value is number {
  return Number.isSafeInteger(value) && (value as number) >= least;
}

// A run event whose fields this app reads are all there, permission prompts' included.
function isEvent(value: unknown): value is RunEvent {
  const event = record(value);
  const run = typeof event?.["run"] === "string" && Number.isInteger(event["prompt"]);
  switch (event?.["event"]) {
    case "permission_asked":
      return run && typeof event["tool"] === "string";
    case "permission_withdrawn":
      return run;
    default:
      return isRunEvent(value);
  }
}
