import type { ChatRef } from "../chat-ref.ts";
import type { ChatQueryKind, Envelope } from "../protocol.ts";
import { type Chat, commit, fromSnapshot, missing, older, stream } from "./chat.ts";
import type { Heard } from "./heard.ts";

/**
 * Where the chat stands with the server, each query by its envelope id: not waiting, waiting on
 * its snapshot, subscribed (with a window of older events on its way, perhaps), or refused.
 */
type SyncState =
  | { kind: "idle" }
  | { kind: "asking"; snapshot: number }
  | { kind: "live"; subscription: number; window: number | undefined }
  | { kind: "failed" };

/** The chat shown, and where it stands with the server. */
export type Sync = { chat: Chat | undefined; state: SyncState };

/** The queries to send, and the state they leave. */
export type Step = { sync: Sync; send: Envelope[] };

export const START: Sync = { chat: undefined, state: { kind: "idle" } };

// How many older events scrolling up asks for at a time.
const PAGE = 100;

/** Asks for the chat afresh, keeping what it shows until the answer comes, as on opening it. */
export function open(sync: Sync, ref: ChatRef, id: () => number): Step {
  const query = ask(ref, id(), { kind: "snapshot" });
  return { sync: { ...sync, state: { kind: "asking", snapshot: query.id } }, send: [query] };
}

/** Stops waiting for answers, keeping the chat, as when the connection ends. */
export function ended(sync: Sync): Sync {
  const waiting = sync.state.kind === "asking" || sync.state.kind === "live";
  return waiting ? { ...sync, state: { kind: "idle" } } : sync;
}

/** Asks for the page of events before the oldest the chat holds, unless one is on its way. */
export function more(sync: Sync, ref: ChatRef, id: () => number): Step {
  const { chat, state } = sync;
  if (chat === undefined || state.kind !== "live" || state.window !== undefined || chat.first <= 1) {
    return { sync, send: [] };
  }
  return windowTo(chat, state.subscription, ref, id, Math.max(1, chat.first - PAGE));
}

/** What hearing `heard` does to `sync`. */
export function hear(sync: Sync, heard: Heard, ref: ChatRef, id: () => number): Step {
  const { chat, state } = sync;
  switch (heard.kind) {
    case "snapshot": {
      if (state.kind !== "asking" || heard.id !== state.snapshot) {
        return { sync, send: [] };
      }
      const fresh = fromSnapshot(heard.snapshot);
      const subscribe = ask(ref, id(), { kind: "subscribe", after: fresh.seq });
      return fetchWaiting(fresh, subscribe.id, ref, id, [subscribe]);
    }
    case "entry": {
      if (chat === undefined || state.kind !== "live") {
        return { sync, send: [] };
      }
      const next = commit(chat, heard.entry);
      return next === undefined ? open(sync, ref, id) : { sync: { ...sync, chat: next }, send: [] };
    }
    case "lost":
      return state.kind === "live" && heard.id === state.subscription ? open(sync, ref, id) : { sync, send: [] };
    case "window": {
      if (chat === undefined || state.kind !== "live" || heard.id !== state.window) {
        return { sync, send: [] };
      }
      const next = older(chat, heard.events);
      // An answer that adds nothing can't bring the oldest waiting prompt any closer.
      return next === chat
        ? { sync: { chat, state: { ...state, window: undefined } }, send: [] }
        : fetchWaiting(next, state.subscription, ref, id, []);
    }
    case "refused":
      if (state.kind === "asking" ? heard.id === state.snapshot : state.kind === "live" && heard.id === state.subscription) {
        // A window on its way goes with the subscription; asking again starts afresh.
        return { sync: { ...sync, state: { kind: "failed" } }, send: [] };
      }
      if (state.kind === "live" && heard.id === state.window) {
        return { sync: { ...sync, state: { ...state, window: undefined } }, send: [] };
      }
      return { sync, send: [] };
    case "run":
      return chat === undefined ? { sync, send: [] } : { sync: { ...sync, chat: stream(chat, heard.run, heard.event) }, send: [] };
  }
}

// `chat`, subscribed under `subscription`, asking for every event back to the oldest waiting
// prompt it doesn't hold yet.
function fetchWaiting(chat: Chat, subscription: number, ref: ChatRef, id: () => number, send: Envelope[]): Step {
  const oldest = missing(chat);
  if (oldest === undefined) {
    return { sync: { chat, state: { kind: "live", subscription, window: undefined } }, send };
  }
  const step = windowTo(chat, subscription, ref, id, oldest);
  return { sync: step.sync, send: [...send, ...step.send] };
}

function windowTo(chat: Chat, subscription: number, ref: ChatRef, id: () => number, from: number): Step {
  const query = ask(ref, id(), { kind: "window", before: chat.first, limit: chat.first - from });
  return { sync: { chat, state: { kind: "live", subscription, window: query.id } }, send: [query] };
}

function ask(ref: ChatRef, id: number, query: ChatQueryKind): Envelope {
  return { id, type: "chat_query", ...ref, query };
}
