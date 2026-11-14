import type { ChatRef } from "../chat-ref.ts";
import type { ChatMessage, ErrorMessage, Incoming, RunMessage } from "../incoming.ts";
import type { ChatEntry, RunEvent, RunId } from "../protocol.ts";
import type { Run, Snapshot } from "./chat.ts";

/** What the server said about one chat. */
export type Heard =
  | { kind: "entry"; entry: ChatEntry }
  | { kind: "lost" }
  | { kind: "snapshot"; id: number; snapshot: Snapshot }
  | { kind: "window"; id: number; events: ChatEntry[] }
  | { kind: "run"; run: RunId; event: RunEvent }
  | { kind: "refused"; id: number };

/**
 * What `message` says about the chat `ref`, or `undefined` when it is about another one. An error
 * is heard by its envelope id, which says which query, if any, it refuses.
 */
export function heard(message: ChatMessage | RunMessage | ErrorMessage, ref: ChatRef): Heard | undefined {
  if (message.type === "error") {
    return { kind: "refused", id: message.id };
  }
  if (message.type === "run") {
    return { kind: "run", run: message.run, event: message.event };
  }
  if (message.workspace !== ref.workspace || message.chat !== ref.chat) {
    return undefined;
  }
  if (message.type === "chat") {
    return { kind: "entry", entry: message.entry };
  }
  const { answer } = message;
  switch (answer.kind) {
    case "events_lost":
      return { kind: "lost" };
    case "window":
      return { kind: "window", id: message.id, events: answer.events };
    case "snapshot": {
      const runs = new Map<RunId, Run>(answer.runs.map(({ run, state, agent }) => [run, { state, agent }]));
      return { kind: "snapshot", id: message.id, snapshot: { seq: answer.seq, runs, waiting: answer.waiting, events: answer.events } };
    }
  }
}

/**
 * The receipt a committed entry carries, as commands wait for one: a core's receipts are events in
 * its chat's log.
 */
export function receiptIn(message: ChatMessage | RunMessage | ErrorMessage): Extract<Incoming, { type: "receipt" }> | undefined {
  if (message.type !== "chat" || message.entry.event.kind !== "receipt") {
    return undefined;
  }
  return { id: message.id, type: "receipt", ...message.entry.event.receipt };
}
