import type { App, Envelope, RunEvent } from "./protocol.ts";

/** A message from the server that this app acts on, its fields checked where it arrives. */
export type Incoming = Extract<Envelope, { type: "welcome" | "error" | "receipt" | "run" | "diff_answer" | "workspace" }>;

/** A change to a workspace, from its feed, checked. */
export type WorkspaceMessage = Extract<Incoming, { type: "workspace" }>;

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
    case "workspace":
      return text("workspace") && Number.isSafeInteger(fields["seq"]) && isChange(fields["change"]);
    case "error":
      return text("code");
    case "receipt":
      return text("request") && text("state");
    case "run":
      return text("run") && isRunEvent(fields["event"]);
    case "diff_answer":
      // The answer itself is checked where the chat's changes take it.
      return text("request") && text("workspace") && text("chat");
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
  const fields = value as Record<string, unknown>;
  return fields["kind"] === "chat_changed" ? typeof fields["chat"] === "string" : typeof fields["kind"] === "string";
}
