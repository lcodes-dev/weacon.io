import { type Status, offers } from "../connection.ts";
import type { ChatId, DiffFile, Envelope, Hunk, LineOp, RequestId, WorkspaceId } from "../protocol.ts";
import { record } from "../fields.ts";

/** What the core has sent of a chat's changes under the latest request. */
export type Changes = {
  asked: { id: number; request: RequestId; workspace: WorkspaceId; chat: ChatId } | undefined;
  files: readonly DiffFile[];
  /** How many files the change has, once the first answer says. */
  total: number | undefined;
  /** Whether the connection ended, or the query failed, before every file came. */
  cut: boolean;
  /** Whether `files` and `total` are an earlier request's, shown until this one's first answer. */
  held: boolean;
};

export const NO_CHANGES: Changes = { asked: undefined, files: [], total: undefined, cut: false, held: false };

/** Whether `status` is a connection that can ask for changes. */
export function canAsk(status: Status): boolean {
  return offers(status, "diff.view");
}

/** The query, as message `id`, for `chat`'s changes under `request`; what is `shown` stays until its first answer. */
export function ask(
  id: number,
  request: RequestId,
  workspace: WorkspaceId,
  chat: ChatId,
  shown: Changes,
): { query: Envelope; changes: Changes } {
  return {
    query: { id, type: "diff_query", request, workspace, chat, query: { kind: "changes" } },
    changes: { ...shown, asked: { id, request, workspace, chat }, cut: false, held: shown.files.length > 0 },
  };
}

/** Whether opening the changes should ask for them: they never were, or the last answers were cut short. */
export function needsAsking(changes: Changes): boolean {
  return changes.asked === undefined || changes.cut;
}

/** `changes` with `envelope` taken in when it answers the latest request; a stale, extra or malformed one is dropped. */
export function receive(changes: Changes, envelope: Envelope): Changes {
  const { asked } = changes;
  const { files, total } = changes.held ? NO_CHANGES : changes;
  // An error under the query's id answers it.
  if (envelope.type === "error" && asked !== undefined && envelope.id === asked.id) {
    return cut(changes);
  }
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
    return total === undefined ? { ...changes, files: [], total: 0, held: false } : changes;
  }
  if (!isFileAnswer(answer)) {
    return changes;
  }
  return { ...changes, files: [...files, answer.file], total: total ?? answer.files, held: false };
}

/** `changes` once the connection has ended: any files still to come won't. */
export function cut(changes: Changes): Changes {
  const { asked, held, files, total } = changes;
  const waiting = asked !== undefined && (held || total === undefined || files.length < total);
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

