import { record } from "./fields.ts";
import type { App, ChatEntry, Envelope, RunEvent } from "./protocol.ts";

/** A message from the server that this app acts on, its fields checked where it arrives. */
export type Incoming = Extract<
  Envelope,
  {
    type: "welcome" | "error" | "receipt" | "run" | "diff_answer" | "chat" | "chat_answer" | "workspace" | "machine_settings_answer" | "sessions_answer" | "symbols_answer" | "worktree_answer" | "terminal" | "chat_live";
  }
>;

/** A run message, checked. */
export type RunMessage = Extract<Incoming, { type: "run" }>;

/** A chat's committed event, an answer about its log, or one of its runs' live updates, checked. */
export type ChatMessage = Extract<Incoming, { type: "chat" | "chat_answer" | "chat_live" }>;

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
  run?(message: RunMessage): void;
  answers?(message: Extract<Incoming, { type: "receipt" | "error" }>): void;
  changes?(message: Extract<Incoming, { type: "diff_answer" | "error" }>): void;
  chat?(message: ChatMessage | ErrorMessage): void;
  worktree?(message: Extract<Incoming, { type: "worktree_answer" | "error" }>): void;
};

/** Hands `message` to the handlers it may be for; an error may answer any of their requests. */
export function route(message: Incoming, to: Routes): void {
  switch (message.type) {
    case "run":
      to.run?.(message);
      return;
    case "receipt":
      to.answers?.(message);
      return;
    case "diff_answer":
      to.changes?.(message);
      return;
    case "worktree_answer":
      to.worktree?.(message);
      return;
    case "chat":
    case "chat_answer":
    case "chat_live":
      to.chat?.(message);
      return;
    case "error":
      to.answers?.(message);
      to.changes?.(message);
      to.chat?.(message);
      to.worktree?.(message);
      return;
    case "welcome":
    case "workspace":
    case "machine_settings_answer":
    case "sessions_answer":
    case "symbols_answer":
    case "terminal":
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
  diff_answer: (fields) =>
    text(fields, "request") && text(fields, "workspace") && text(fields, "chat") && isDiffAnswer(fields["answer"]),
  chat: (fields) => text(fields, "workspace") && text(fields, "chat") && isEntry(fields["entry"]),
  chat_answer: (fields) => text(fields, "workspace") && text(fields, "chat") && isChatAnswer(fields["answer"]),
  chat_live: (fields) =>
    text(fields, "workspace") && text(fields, "chat") && isRunUpdate(fields["update"]) && (fields["after"] === undefined || isSeq(fields["after"], 0)),
  workspace: (fields) => text(fields, "workspace") && Number.isSafeInteger(fields["seq"]) && isChange(fields["change"]),
  worktree_answer: (fields) =>
    text(fields, "request") && text(fields, "workspace") && text(fields, "chat") && isWorktreeAnswer(fields["answer"]),
  machine_settings_answer: (fields) =>
    text(fields, "request") && text(fields, "machine") && text(fields, "workspace") && isMachineSettingsAnswer(fields["answer"]),
  sessions_answer: (fields) => text(fields, "request") && text(fields, "machine") && isSessionsAnswer(fields["answer"]),
  symbols_answer: (fields) => text(fields, "request") && text(fields, "machine") && text(fields, "workspace") && isSymbolsAnswer(fields["answer"]),
  terminal: (fields) => text(fields, "terminal") && text(fields, "workspace") && isTerminalEvent(fields["event"]),
};

// A terminal event of a kind this app knows, with its fields; a newer kind would be read as a frame.
function isTerminalEvent(value: unknown): boolean {
  const event = record(value);
  if (event === undefined) {
    return false;
  }
  switch (event["kind"]) {
    case "accepted":
      return text(event, "handshake");
    case "frame":
      return text(event, "sealed");
    case "closed":
      return event["code"] === undefined || text(event, "code");
    default:
      return false;
  }
}

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
  switch (fields["event"]) {
    case "text_delta":
    case "text":
      return text(fields, "text");
    case "tool_call":
      return text(fields, "call") && text(fields, "tool");
    case "tool_done":
      return text(fields, "call") && typeof fields["failed"] === "boolean";
    case "permission_asked":
      return text(fields, "run") && isSeq(fields["prompt"], 0) && text(fields, "tool");
    case "permission_withdrawn":
      return text(fields, "run") && isSeq(fields["prompt"], 0);
    case "permission_answered": {
      const answer = record(fields["answer"]);
      return answer !== undefined && text(answer, "run") && isSeq(answer["prompt"], 0) && typeof answer["allow"] === "boolean";
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
      // A malformed note is dropped as the message is parsed: rejecting the prompt would leave a gap.
      if (event["pick"] !== undefined && !isPickNote(event["pick"])) {
        delete event["pick"];
      }
      return typeof event["request"] === "string" && typeof event["text"] === "string";
    case "message":
      return typeof event["author"] === "string" && typeof event["text"] === "string";
    case "outside_prompt":
      return typeof event["text"] === "string";
    case "receipt":
      return typeof record(event["receipt"])?.["state"] === "string";
    case "run":
      return isRunUpdate(event["update"]);
    case "run_status":
      return isStatus(event["status"]);
    default:
      return true;
  }
}

function isPickNote(value: unknown): boolean {
  const note = record(value);
  return ["url", "title", "selector"].every((key) => typeof note?.[key] === "string");
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

/** Whether `answer` is the current settings with every field this app reads, or another kind it ignores. */
function isMachineSettingsAnswer(answer: unknown): boolean {
  const fields = record(answer);
  if (fields === undefined) {
    return false;
  }
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
      const one = record(signIn);
      return one !== undefined && text(one, "agent") && text(one, "state");
    })
  );
}

// A state needs its three flags and a page its https address; a newer kind passes, to be ignored.
function isWorktreeAnswer(answer: unknown): boolean {
  const fields = record(answer);
  switch (fields?.["kind"]) {
    case "state":
      return ["removed", "uncommitted", "unpushed"].every((key) => typeof fields[key] === "boolean");
    case "pull_request_opened":
    case "pull_request_page":
      return isHttps(fields["url"]);
    default:
      return typeof fields?.["kind"] === "string";
  }
}

function isHttps(url: unknown): boolean {
  return typeof url === "string" && URL.canParse(url) && new URL(url).protocol === "https:";
}

// An answer of a kind this app reads has every field the diff view reads; others are dropped.
function isDiffAnswer(answer: unknown): boolean {
  const fields = record(answer);
  if (fields?.["kind"] === "unchanged") {
    return true;
  }
  const file = record(fields?.["file"]);
  const files = fields?.["files"];
  return (
    fields?.["kind"] === "file" &&
    typeof files === "number" &&
    Number.isSafeInteger(files) &&
    files >= 1 &&
    typeof file?.["path"] === "string" &&
    isFileChange(file["change"]) &&
    isContent(file["content"])
  );
}

function isFileChange(change: unknown): boolean {
  const fields = record(change);
  const kind = fields?.["kind"];
  return typeof kind === "string" && (kind !== "renamed" || typeof fields?.["from"] === "string");
}

function isContent(content: unknown): boolean {
  const fields = record(content);
  switch (fields?.["kind"]) {
    case "text": {
      const hunks = fields["hunks"];
      return Array.isArray(hunks) && hunks.every(isHunk);
    }
    default:
      return typeof fields?.["kind"] === "string";
  }
}

function isHunk(hunk: unknown): boolean {
  const fields = record(hunk);
  const lines = fields?.["lines"];
  return (
    Number.isSafeInteger(fields?.["old_start"]) &&
    Number.isSafeInteger(fields?.["new_start"]) &&
    Array.isArray(lines) &&
    lines.every((line) => {
      const parts = record(line);
      return typeof parts?.["op"] === "string" && typeof parts["text"] === "string";
    })
  );
}

function isSessionsAnswer(answer: unknown): boolean {
  const fields = record(answer);
  if (fields === undefined) {
    return false;
  }
  switch (fields["kind"]) {
    case "list": {
      const sessions = fields["sessions"];
      return Array.isArray(sessions) && sessions.every(isOutsideSession);
    }
    case "opened": {
      const events = fields["events"];
      return Array.isArray(events) && events.every(isEntry) && typeof fields["more"] === "boolean";
    }
    default:
      return typeof fields["kind"] === "string";
  }
}

function isSymbolsAnswer(answer: unknown): boolean {
  const fields = record(answer);
  if (fields?.["kind"] !== "found") {
    return typeof fields?.["kind"] === "string";
  }
  const symbols = fields["symbols"];
  return Array.isArray(symbols) && symbols.every(isSymbol) && typeof fields["more"] === "boolean";
}

function isSymbol(symbol: unknown): boolean {
  const fields = record(symbol);
  return (
    fields !== undefined &&
    ["repo", "kind", "name", "path"].every((key) => text(fields, key)) &&
    isSeq(fields["line"], 1)
  );
}

function isOutsideSession(session: unknown): boolean {
  const fields = record(session);
  return (
    fields !== undefined &&
    text(fields, "agent") &&
    text(fields, "session") &&
    Number.isSafeInteger(fields["modified_at"]) &&
    (fields["folder"] === undefined || text(fields, "folder"))
  );
}

function isRunUpdate(value: unknown): boolean {
  const update = record(value);
  return update !== undefined && text(update, "run") && isRunEvent(update["event"]);
}
