import type { Envelope } from "./protocol.ts";

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
  receipt: (fields) => text(fields, "request") && text(fields, "state"),
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

function isLocalChat(value: unknown): boolean {
  const chat = record(value);
  return (
    chat !== undefined &&
    ["workspace", "chat", "title", "agent"].every((key) => text(chat, key)) &&
    Number.isSafeInteger(chat["updated_at"]) &&
    (chat["run"] === undefined || text(chat, "run"))
  );
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
      const statuses = Array.isArray(runs) && runs.every((run) => text(record(run), "run") && text(record(run), "state"));
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
      return text(record(event["receipt"]), "state");
    case "run": {
      const update = record(event["update"]);
      return text(update, "run") && isRunEvent(update?.["event"]);
    }
    case "run_status": {
      const status = record(event["status"]);
      return text(status, "run") && text(status, "state");
    }
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
    case "text":
      return text(event, "text");
    case "tool_call":
      return text(event, "call") && text(event, "tool");
    case "tool_done":
      return text(event, "call") && typeof event["failed"] === "boolean";
    case "permission_asked":
      return text(event, "run") && isSeq(event["prompt"], 0) && text(event, "tool");
    case "permission_withdrawn":
      return text(event, "run") && isSeq(event["prompt"], 0);
    case "permission_answered": {
      const answer = record(event["answer"]);
      return text(answer, "run") && isSeq(answer?.["prompt"], 0) && typeof answer?.["allow"] === "boolean";
    }
    case "finished":
      return text(record(event["outcome"]), "state");
    default:
      return typeof event?.["event"] === "string";
  }
}
