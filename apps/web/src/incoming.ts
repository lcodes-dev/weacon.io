import type { App, Envelope, RunEvent } from "./protocol.ts";

/** A message from the server that this app acts on, its fields checked where it arrives. */
export type Incoming = Extract<Envelope, { type: "welcome" | "error" | "receipt" | "run" | "diff_answer" | "workspace" | "machine_settings_answer" }>;

/** A run message, checked. */
export type RunMessage = Extract<Incoming, { type: "run" }>;

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
    case "error":
      to.answers(message);
      to.changes(message);
      return;
    case "welcome":
    case "workspace":
    case "machine_settings_answer":
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
  workspace: (fields) => text(fields, "workspace") && Number.isSafeInteger(fields["seq"]) && isChange(fields["change"]),
  machine_settings_answer: (fields) =>
    text(fields, "request") && text(fields, "machine") && text(fields, "workspace") && isMachineSettingsAnswer(fields["answer"]),
};

function isIncoming(value: unknown): value is Incoming {
  if (typeof value !== "object" || value === null) {
    return false;
  }
  const fields = value as Fields;
  const type = fields["type"];
  return (
    typeof fields["id"] === "number" &&
    typeof type === "string" &&
    Object.hasOwn(CHECKS, type) &&
    CHECKS[type as Incoming["type"]](fields)
  );
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
    case "finished": {
      const outcome = fields["outcome"] as Record<string, unknown> | null | undefined;
      return typeof outcome === "object" && outcome !== null && typeof outcome["state"] === "string";
    }
    default:
      return typeof fields["event"] === "string";
  }
}

// A change of a kind this app reads has its fields; one of a newer kind passes, to be skipped.
function isChange(value: unknown): boolean {
  if (typeof value !== "object" || value === null) {
    return false;
  }
  const fields = value as Fields;
  return fields["kind"] === "chat_changed" ? typeof fields["chat"] === "string" : typeof fields["kind"] === "string";
}

/** Whether `answer` is the current settings with every field this app reads, or another kind it ignores. */
function isMachineSettingsAnswer(answer: unknown): boolean {
  if (typeof answer !== "object" || answer === null) {
    return false;
  }
  const fields = answer as Fields;
  if (fields["kind"] !== "current") {
    return typeof fields["kind"] === "string";
  }
  const signIns = fields["sign_ins"];
  return (
    text(fields, "access") &&
    typeof fields["team"] === "boolean" &&
    (fields["reports"] === undefined || typeof fields["reports"] === "boolean") &&
    Array.isArray(signIns) &&
    signIns.every((signIn: unknown) => {
      const one = signIn as Fields | null;
      return typeof one === "object" && one !== null && text(one, "agent") && text(one, "state");
    })
  );
}
