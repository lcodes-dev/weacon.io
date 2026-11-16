import type { AgentKind, Envelope, Outcome, RunState } from "./protocol.ts";

/** A message from the core that the panel acts on, its fields checked where it arrives. */
export type Incoming = Extract<Envelope, { type: "welcome" | "error" | "receipt" | "chat" | "chat_answer" | "chats_answer" }>;

type Fields = Record<string, unknown>;

/** The message `value` holds, or `undefined` when it holds none the panel acts on, or a malformed one. */
export function parse(value: unknown): Incoming | undefined {
  const fields = record(value);
  const type = fields?.["type"];
  if (fields === undefined || !isSeq(fields["id"], 0) || typeof type !== "string" || !Object.hasOwn(CHECKS, type)) {
    return undefined;
  }
  return CHECKS[type as Incoming["type"]](fields) ? (fields as Incoming) : undefined;
}

const CHECKS: { [T in Incoming["type"]]: (fields: Fields) => boolean } = {
  welcome: (fields) => isApp(fields["app"]) && (fields["capabilities"] === undefined || isTexts(fields["capabilities"])),
  error: (fields) => text(fields, "code"),
  receipt: (fields) => text(fields, "request") && isOutcome(fields),
  chat: (fields) => isAbout(fields) && isEntry(fields["entry"]),
  chat_answer: (fields) => isAbout(fields) && isChatAnswer(fields["answer"]),
  chats_answer: (fields) => {
    const answer = record(fields["answer"]);
    return answer?.["kind"] === "list" && Array.isArray(answer["chats"]) && answer["chats"].every(isLocalChat);
  },
};

function record(value: unknown): Fields | undefined {
  return typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Fields) : undefined;
}

function text(fields: Fields | undefined, key: string): boolean {
  return typeof fields?.[key] === "string";
}

function isTexts(value: unknown): boolean {
  return Array.isArray(value) && value.every((item) => typeof item === "string");
}

function isWants(value: unknown): boolean {
  const wants = record(value);
  return text(wants, "text") && (wants?.["cut"] === undefined || typeof wants["cut"] === "boolean");
}

function isSeq(value: unknown, least: number): value is number {
  return Number.isSafeInteger(value) && (value as number) >= least;
}

function isApp(value: unknown): boolean {
  const app = record(value);
  return text(app, "name") && text(app, "version") && text(app, "commit");
}

function isAbout(fields: Fields): boolean {
  return text(fields, "workspace") && text(fields, "chat");
}

const AGENT_KINDS: readonly string[] = ["claude", "codex"] satisfies AgentKind[];
const RUN_STATES: readonly string[] = ["starting", "running", "waiting", "idle", "sleeping", "ended"] satisfies RunState[];
const OUTCOMES: readonly string[] = ["accepted", "completed", "interrupted", "failed", "unknown"] satisfies Outcome["state"][];

function isLocalChat(value: unknown): boolean {
  const chat = record(value);
  return (
    chat !== undefined &&
    ["workspace", "chat", "title"].every((key) => text(chat, key)) &&
    AGENT_KINDS.includes(chat["agent"] as string) &&
    Number.isSafeInteger(chat["updated_at"]) &&
    (chat["run"] === undefined || RUN_STATES.includes(chat["run"] as string))
  );
}

function isStatus(value: unknown): boolean {
  const status = record(value);
  return text(status, "run") && RUN_STATES.includes(status?.["state"] as string) && (status?.["agent"] === undefined || isAgent(status["agent"]));
}

function isAgent(value: unknown): boolean {
  const agent = record(value);
  const owner = record(agent?.["owner"]);
  const owned = (owner?.["kind"] === "shared" && text(owner, "member") && text(owner, "owner")) || (owner?.["kind"] === "member" && text(owner, "member"));
  return AGENT_KINDS.includes(agent?.["kind"] as string) && owned && (agent?.["machine"] === undefined || text(agent, "machine"));
}

function isOutcome(value: unknown): boolean {
  const outcome = record(value);
  return OUTCOMES.includes(outcome?.["state"] as string);
}

// An answer of a kind the panel knows has every field it reads; others are dropped.
function isChatAnswer(value: unknown): boolean {
  const answer = record(value);
  const events = answer?.["events"];
  const entries = Array.isArray(events) && events.every(isEntry);
  switch (answer?.["kind"]) {
    case "events_lost":
      return true;
    case "window":
      return entries;
    case "snapshot": {
      const { seq, runs, waiting } = answer;
      const statuses = Array.isArray(runs) && runs.every(isStatus);
      return entries && isSeq(seq, 0) && statuses && Array.isArray(waiting) && waiting.every((at) => isSeq(at, 1));
    }
    default:
      return false;
  }
}

// An entry whose event the panel reads has every field it reads; one of a newer kind keeps its place.
function isEntry(value: unknown): boolean {
  const entry = record(value);
  const event = record(entry?.["event"]);
  if (entry === undefined || !isSeq(entry["seq"], 1) || event === undefined || typeof event["kind"] !== "string") {
    return false;
  }
  switch (event["kind"]) {
    case "prompt":
      return text(event, "request") && text(event, "text") && (event["pick"] === undefined || isNote(event["pick"]));
    case "outside_prompt":
      return text(event, "text");
    case "receipt":
      return isOutcome(event["receipt"]);
    case "run": {
      const update = record(event["update"]);
      return text(update, "run") && isRunEvent(update?.["event"]);
    }
    case "run_status":
      return isStatus(event["status"]);
    default:
      return true;
  }
}

function isNote(value: unknown): boolean {
  const note = record(value);
  return text(note, "url") && text(note, "title") && text(note, "selector");
}

function isRunEvent(value: unknown): boolean {
  const event = record(value);
  switch (event?.["event"]) {
    case "started":
      return text(event, "session");
    case "text_delta":
    case "text":
      return text(event, "text");
    case "tool_call":
      return text(event, "call") && text(event, "tool");
    case "tool_done":
      return text(event, "call") && typeof event["failed"] === "boolean";
    case "permission_asked":
      return text(event, "run") && isSeq(event["prompt"], 0) && text(event, "tool") && (event["wants"] === undefined || isWants(event["wants"]));
    case "permission_withdrawn":
      return text(event, "run") && isSeq(event["prompt"], 0);
    case "permission_answered": {
      const answer = record(event["answer"]);
      return text(answer, "run") && isSeq(answer?.["prompt"], 0) && typeof answer?.["allow"] === "boolean";
    }
    case "finished":
      return isOutcome(event["outcome"]);
    default:
      return false;
  }
}
