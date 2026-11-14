import { record } from "./fields.ts";
import type { App, ChatEntry, Envelope, RunEvent } from "./protocol.ts";

/** A message from the server that this app acts on, its fields checked where it arrives. */
export type Incoming = Extract<
  Envelope,
  { type: "welcome" | "error" | "receipt" | "run" | "diff_answer" | "chat" | "chat_answer" | "workspace" }
>;

/** A run message, checked. */
export type RunMessage = Extract<Incoming, { type: "run" }>;

/** A chat's committed event or an answer about its log, checked. */
export type ChatMessage = Extract<Incoming, { type: "chat" | "chat_answer" }>;

/** An error, which may answer a query, a command or a diff query. */
export type ErrorMessage = Extract<Incoming, { type: "error" }>;

/** The message `data` holds, or `undefined` when it holds none this app acts on, or a malformed one. */
export function parse(data: unknown): Incoming | undefined {
  if (typeof data !== "string") {
    return undefined;
  }
  let value: unknown;
  try {
    value = JSON.parse(data);
  } catch {
    return undefined;
  }
  return isIncoming(value) ? value : undefined;
}

/** Where incoming messages go: each handler takes only the messages that may be for it. */
export type Routes = {
  run(message: RunMessage): void;
  answers(message: Extract<Incoming, { type: "receipt" | "error" }>): void;
  changes(message: Extract<Incoming, { type: "diff_answer" | "error" }>): void;
  chat(message: ChatMessage | ErrorMessage): void;
};

/** Hands `message` to the handlers it may be for; an error may answer a command, a chat query or the diff query. */
export function route(message: Incoming, to: Routes): void {
  switch (message.type) {
    case "run":
      to.run(message);
      return;
    case "receipt":
      to.answers(message);
      return;
    case "diff_answer":
      to.changes(message);
      return;
    case "chat":
    case "chat_answer":
      to.chat(message);
      return;
    case "error":
      to.answers(message);
      to.changes(message);
      to.chat(message);
      return;
    case "welcome":
    case "workspace":
      return;
  }
}

type Fields = Record<string, unknown>;

const text = (fields: Fields, key: string) => typeof fields[key] === "string";

// The fields each message this app acts on must have.
const CHECKS: { [T in Incoming["type"]]: (fields: Fields) => boolean } = {
  welcome: (fields) => isApp(fields["app"]) && (fields["capabilities"] === undefined || isStrings(fields["capabilities"])),
  error: (fields) => text(fields, "code"),
  receipt: (fields) => text(fields, "request") && text(fields, "state"),
  run: (fields) => text(fields, "run") && isRunEvent(fields["event"]),
  // The answer itself is checked where the chat's changes take it.
  diff_answer: (fields) => text(fields, "request") && text(fields, "workspace") && text(fields, "chat"),
  chat: (fields) => text(fields, "workspace") && text(fields, "chat") && isEntry(fields["entry"]),
  chat_answer: (fields) => text(fields, "workspace") && text(fields, "chat") && isChatAnswer(fields["answer"]),
  workspace: (fields) => text(fields, "workspace") && Number.isSafeInteger(fields["seq"]) && isChange(fields["change"]),
};

function isIncoming(value: unknown): value is Incoming {
  const fields = record(value);
  const type = fields?.["type"];
  return (
    typeof fields?.["id"] === "number" &&
    typeof type === "string" &&
    Object.hasOwn(CHECKS, type) &&
    CHECKS[type as Incoming["type"]](fields)
  );
}

function isApp(app: unknown): app is App {
  const fields = record(app);
  return ["name", "version", "commit"].every((key) => typeof fields?.[key] === "string");
}

function isStrings(list: unknown): list is string[] {
  return Array.isArray(list) && list.every((item) => typeof item === "string");
}

/** Whether `event` is a run event whose fields this app reads are all there; others it ignores. */
function isRunEvent(event: unknown): event is RunEvent {
  const fields = record(event);
  if (fields === undefined) {
    return false;
  }
  const text = (key: string) => typeof fields[key] === "string";
  switch (fields["event"]) {
    case "text_delta":
    case "text":
      return text("text");
    case "tool_call":
      return text("call") && text("tool");
    case "tool_done":
      return text("call") && typeof fields["failed"] === "boolean";
    case "permission_asked":
      return text("run") && isSeq(fields["prompt"], 0) && text("tool");
    case "permission_withdrawn":
      return text("run") && isSeq(fields["prompt"], 0);
    case "permission_answered": {
      const answer = record(fields["answer"]);
      return typeof answer?.["run"] === "string" && isSeq(answer["prompt"], 0) && typeof answer["allow"] === "boolean";
    }
    case "finished":
      return typeof record(fields["outcome"])?.["state"] === "string";
    default:
      return typeof fields["event"] === "string";
  }
}

function isSeq(value: unknown, least: number): value is number {
  return Number.isSafeInteger(value) && (value as number) >= least;
}

// An answer of a kind this app knows has every field it reads; others are dropped.
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
      return (
        entries &&
        isSeq(seq, 0) &&
        Array.isArray(runs) &&
        runs.every(isStatus) &&
        Array.isArray(waiting) &&
        waiting.every((at) => isSeq(at, 1))
      );
    }
    default:
      return false;
  }
}

// An entry whose event this app reads has every field it reads; one of a newer kind keeps its place.
function isEntry(value: unknown): value is ChatEntry {
  const entry = record(value);
  const event = record(entry?.["event"]);
  if (entry === undefined || !isSeq(entry["seq"], 1) || event === undefined || typeof event["kind"] !== "string") {
    return false;
  }
  switch (event["kind"]) {
    case "prompt":
      return typeof event["request"] === "string" && typeof event["text"] === "string";
    case "receipt":
      return typeof record(event["receipt"])?.["state"] === "string";
    case "run": {
      const update = record(event["update"]);
      return typeof update?.["run"] === "string" && isRunEvent(update["event"]);
    }
    case "run_status":
      return isStatus(event["status"]);
    default:
      return true;
  }
}

// A malformed agent, or one whose owner is of a newer kind, is dropped as the message is parsed:
// rejecting the status would leave a gap in the chat's log.
function isStatus(value: unknown): boolean {
  const status = record(value);
  if (typeof status?.["run"] !== "string" || typeof status["state"] !== "string") {
    return false;
  }
  if (status["agent"] !== undefined && !isAgent(status["agent"])) {
    delete status["agent"];
  }
  return true;
}

function isAgent(value: unknown): boolean {
  const agent = record(value);
  const owner = record(agent?.["owner"]);
  const known =
    owner?.["kind"] === "shared" || (owner?.["kind"] === "member" && typeof owner["member"] === "string");
  return typeof agent?.["kind"] === "string" && known && (agent["machine"] === undefined || typeof agent["machine"] === "string");
}

// A change of a kind this app reads has its fields; one of a newer kind passes, to be skipped.
function isChange(value: unknown): boolean {
  const fields = record(value);
  return fields?.["kind"] === "chat_changed" ? typeof fields["chat"] === "string" : typeof fields?.["kind"] === "string";
}
