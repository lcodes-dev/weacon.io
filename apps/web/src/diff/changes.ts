import type { Status } from "../connection.ts";
import type { ChatId, DiffFile, Envelope, Hunk, LineOp, RequestId, WorkspaceId } from "../protocol.ts";

/** The capability under which a server answers diff queries. */
export const DIFF_VIEW = "diff.view";

/** What the core has sent of a chat's changes under the latest request. */
export type Changes = {
  asked: { request: RequestId; workspace: WorkspaceId; chat: ChatId } | undefined;
  files: readonly DiffFile[];
  /** How many files the change has, once the first answer says. */
  total: number | undefined;
  /** Whether the connection ended before every file came. */
  cut: boolean;
};

export const NO_CHANGES: Changes = { asked: undefined, files: [], total: undefined, cut: false };

/** Whether `status` is a connection that can ask for changes. */
export function canAsk(status: Status): boolean {
  return status.state === "live" && status.capabilities.includes(DIFF_VIEW);
}

/** The query for `chat`'s changes under `request`, and what to show until its answers come. */
export function ask(request: RequestId, workspace: WorkspaceId, chat: ChatId): { query: Envelope; changes: Changes } {
  return {
    query: { id: 0, type: "diff_query", request, workspace, chat, query: { kind: "changes" } },
    changes: { ...NO_CHANGES, asked: { request, workspace, chat } },
  };
}

/** `changes` with `envelope` taken in when it answers the latest request; a stale, extra or malformed one is dropped. */
export function receive(changes: Changes, envelope: Envelope): Changes {
  const { asked, files, total } = changes;
  if (
    envelope.type !== "diff_answer" ||
    asked === undefined ||
    envelope.request !== asked.request ||
    envelope.workspace !== asked.workspace ||
    envelope.chat !== asked.chat ||
    (total !== undefined && files.length >= total)
  ) {
    return changes;
  }
  const answer: unknown = envelope.answer;
  if (record(answer)?.["kind"] === "unchanged") {
    return { ...changes, total: 0 };
  }
  if (!isFileAnswer(answer)) {
    return changes;
  }
  return { ...changes, files: [...files, answer.file], total: total ?? answer.files };
}

/** `changes` once the connection has ended: any files still to come won't. */
export function cut(changes: Changes): Changes {
  const waiting = changes.asked !== undefined && (changes.total === undefined || changes.files.length < changes.total);
  return waiting ? { ...changes, cut: true } : changes;
}

/** One row of a file's diff: a hunk's start, or a line with its number on each side it is on. */
export type Row =
  | { kind: "hunk"; old: number; new: number }
  | { kind: "line"; op: LineOp; old: number | undefined; new: number | undefined; text: string; noNewline: boolean };

/** The rows of `hunks`, in order. */
export function rows(hunks: readonly Hunk[]): Row[] {
  return hunks.flatMap((hunk): Row[] => {
    let [old, next] = [hunk.old_start, hunk.new_start];
    const lines = hunk.lines.map((line): Row => {
      const row: Row = {
        kind: "line",
        op: line.op,
        old: line.op === "added" ? undefined : old,
        new: line.op === "removed" ? undefined : next,
        text: line.text,
        noNewline: line.no_newline === true,
      };
      old += line.op === "added" ? 0 : 1;
      next += line.op === "removed" ? 0 : 1;
      return row;
    });
    return [{ kind: "hunk", old: hunk.old_start, new: hunk.new_start }, ...lines];
  });
}

// The answer's fields are checked here, where it arrives; the view then trusts them.
function isFileAnswer(answer: unknown): answer is { kind: "file"; files: number; file: DiffFile } {
  const fields = record(answer);
  const file = record(fields?.["file"]);
  const files = fields?.["files"];
  return (
    fields?.["kind"] === "file" &&
    typeof files === "number" &&
    Number.isSafeInteger(files) &&
    files >= 1 &&
    typeof file?.["path"] === "string" &&
    isChange(file["change"]) &&
    isContent(file["content"])
  );
}

function isChange(change: unknown): boolean {
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

function record(value: unknown): Record<string, unknown> | undefined {
  return typeof value === "object" && value !== null ? (value as Record<string, unknown>) : undefined;
}
