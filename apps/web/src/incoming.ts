import type { App, ChatEntry, Envelope, RunEvent } from "./protocol.ts";

/** A message from the server that this app acts on, its fields checked where it arrives. */
export type Incoming = Extract<Envelope, { type: "welcome" | "error" | "receipt" | "run" | "diff_answer" | "chat" | "chat_answer" }>;

/** A run message, checked. */
export type RunMessage = Extract<Incoming, { type: "run" }>;

/** A chat's committed event or an answer about its log, checked. */
export type ChatMessage = Extract<Incoming, { type: "chat" | "chat_answer" }>;

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
  chat(message: ChatMessage): void;
};

/** Hands `message` to the handlers it may be for; an error may answer a prompt or the diff query. */
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
      return;
    case "welcome":
      return;
  }
}

function isIncoming(value: unknown): value is Incoming {
  if (typeof value !== "object" || value === null) {
    return false;
  }
  const fields = value as Record<string, unknown>;
  if (typeof fields["id"] !== "number") {
    return false;
  }
  const text = (key: string) => typeof fields[key] === "string";
  switch (fields["type"]) {
    case "welcome":
      return isApp(fields["app"]) && (fields["capabilities"] === undefined || isStrings(fields["capabilities"]));
    case "error":
      return text("code");
    case "receipt":
      return text("request") && text("state");
    case "run":
      return text("run") && isRunEvent(fields["event"]);
    case "diff_answer":
      // The answer itself is checked where the chat's changes take it.
      return text("request") && text("workspace") && text("chat");
    case "chat":
      return text("workspace") && text("chat") && isEntry(fields["entry"]);
    case "chat_answer":
      return text("workspace") && text("chat") && isChatAnswer(fields["answer"]);
    default:
      return false;
  }
}

function isApp(app: unknown): app is App {
  return (
    typeof app === "object" &&
    app !== null &&
    ["name", "version", "commit"].every((key) => typeof (app as Record<string, unknown>)[key] === "string")
  );
}

function isStrings(list: unknown): list is string[] {
  return Array.isArray(list) && list.every((item) => typeof item === "string");
}

/** Whether `event` is a run event whose fields this app reads are all there; others it ignores. */
function isRunEvent(event: unknown): event is RunEvent {
  if (typeof event !== "object" || event === null) {
    return false;
  }
  const fields = event as Record<string, unknown>;
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
      return text("run") && Number.isInteger(fields["prompt"]) && text("tool");
    case "permission_withdrawn":
      return text("run") && Number.isInteger(fields["prompt"]);
    case "finished": {
      const outcome = fields["outcome"] as Record<string, unknown> | null | undefined;
      return typeof outcome === "object" && outcome !== null && typeof outcome["state"] === "string";
    }
    default:
      return typeof fields["event"] === "string";
  }
}

function record(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null && !Array.isArray(value) ? (value as Record<string, unknown>) : undefined;
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

// A malformed agent, or one whose owner is of a newer kind, is dropped; the status stays.
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
