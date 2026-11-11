import type { Outcome, RunEvent } from "./protocol.ts";

export type ToolState = "running" | "done" | "failed";

/**
 * One row of a run's transcript. A text row keeps its streamed pieces apart, so the view adds a
 * text node for each new piece and never rewrites one a reader may have selected text in.
 */
export type Row =
  | { kind: "text"; pieces: readonly string[] }
  | { kind: "tool"; call: string; tool: string; state: ToolState }
  | { kind: "finished"; outcome: Outcome };

/** The rows of one run, and the text row still being written, if any. */
export type Transcript = { rows: readonly Row[]; writing: boolean };

export const EMPTY: Transcript = { rows: [], writing: false };

/** `transcript` with `events` applied in order; rows they don't touch are the same objects. */
export function apply(transcript: Transcript, events: readonly RunEvent[]): Transcript {
  const rows = [...transcript.rows];
  let writing = transcript.writing;
  for (const event of events) {
    writing = step(rows, writing, event);
  }
  return { rows, writing };
}

// Applies one event to `rows`, giving whether a text row is still open for more.
function step(rows: Row[], writing: boolean, event: RunEvent): boolean {
  switch (event.event) {
    case "text_delta":
    case "text": {
      const last = rows.at(-1);
      const whole = event.event === "text";
      if (!writing || last?.kind !== "text") {
        rows.push({ kind: "text", pieces: [event.text] });
      } else if (!whole) {
        rows[rows.length - 1] = { kind: "text", pieces: [...last.pieces, event.text] };
      } else if (last.pieces.join("") !== event.text) {
        rows[rows.length - 1] = { kind: "text", pieces: [event.text] };
      }
      return !whole;
    }
    case "tool_call":
      rows.push({ kind: "tool", call: event.call, tool: event.tool, state: "running" });
      return false;
    case "tool_done": {
      // A repeat or a stale id changes nothing.
      const at = rows.findLastIndex((row) => row.kind === "tool" && row.call === event.call);
      const row = rows[at];
      if (row?.kind === "tool" && row.state === "running") {
        rows[at] = { ...row, state: event.failed ? "failed" : "done" };
      }
      return writing;
    }
    case "finished":
      rows.push({ kind: "finished", outcome: event.outcome });
      return false;
    default:
      // Other events, and a newer server's, leave the rows as they are.
      return writing;
  }
}

/** Whether `event` is a run event whose fields this app reads are all there; others it ignores. */
export function isRunEvent(event: unknown): event is RunEvent {
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
