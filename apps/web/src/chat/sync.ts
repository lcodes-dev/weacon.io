import type { ChatRef } from "../chat-ref.ts";
import type { ChatQueryKind, Envelope } from "../protocol.ts";
import { type Chat, commit, fromSnapshot, missing, older, stream } from "./chat.ts";
import type { Heard } from "./heard.ts";

/**
 * Where the chat stands with the server: asked for its snapshot, subscribed under the envelope id
 * of each, or refused one of them.
 */
export type SyncState =
  | { kind: "idle" }
  | { kind: "asking"; snapshot: number }
  | { kind: "live"; subscription: number }
  | { kind: "failed" };

/** The chat shown, where it stands, and the window of older events asked for, by envelope id. */
export type Sync = { chat: Chat | undefined; state: SyncState; window: number | undefined };

/** The queries to send, and the state they leave. */
export type Step = { sync: Sync; send: Envelope[] };

export const START: Sync = { chat: undefined, state: { kind: "idle" }, window: undefined };

// How many older events scrolling up asks for at a time.
const PAGE = 100;

/** Asks for the chat afresh, keeping what it shows until the answer comes, as on opening it. */
export function open(sync: Sync, ref: ChatRef, id: () => number): Step {
  const query = ask(ref, id(), { kind: "snapshot" });
  return { sync: { ...sync, state: { kind: "asking", snapshot: query.id }, window: undefined }, send: [query] };
}

/** Stops waiting for answers, keeping the chat, as when the connection ends. */
export function ended(sync: Sync): Sync {
  const waiting = sync.state.kind === "asking" || sync.state.kind === "live";
  return waiting ? { ...sync, state: { kind: "idle" }, window: undefined } : sync;
}

/** Whether the chat is subscribed, so new events reach it. */
export function subscribed(sync: Sync): boolean {
  return sync.state.kind === "live";
}

/** Asks for the page of events before the oldest the chat holds, unless one is on its way. */
export function more(sync: Sync, ref: ChatRef, id: () => number): Step {
  const { chat } = sync;
  if (chat === undefined || !subscribed(sync) || sync.window !== undefined || chat.first <= 1) {
    return { sync, send: [] };
  }
  return windowTo(sync, chat, ref, id, Math.max(1, chat.first - PAGE));
}

/** What hearing `heard` does to `sync`. */
export function hear(sync: Sync, heard: Heard, ref: ChatRef, id: () => number): Step {
  const { chat } = sync;
  switch (heard.kind) {
    case "snapshot": {
      if (sync.state.kind !== "asking" || heard.id !== sync.state.snapshot) {
        return { sync, send: [] };
      }
      const fresh = fromSnapshot(heard.snapshot);
      const subscribe = ask(ref, id(), { kind: "subscribe", after: fresh.seq });
      const next: Sync = { chat: fresh, state: { kind: "live", subscription: subscribe.id }, window: undefined };
      return fetchWaiting(next, ref, id, [subscribe]);
    }
    case "entry": {
      if (chat === undefined || !subscribed(sync)) {
        return { sync, send: [] };
      }
      const next = commit(chat, heard.entry);
      return next === undefined ? open(sync, ref, id) : { sync: { ...sync, chat: next }, send: [] };
    }
    case "lost":
      return sync.state.kind === "live" && heard.id === sync.state.subscription ? open(sync, ref, id) : { sync, send: [] };
    case "window": {
      if (chat === undefined || heard.id !== sync.window) {
        return { sync, send: [] };
      }
      const next = { ...sync, chat: older(chat, heard.events), window: undefined };
      // An answer that adds nothing can't bring the oldest waiting prompt any closer.
      return next.chat === chat ? { sync: next, send: [] } : fetchWaiting(next, ref, id, []);
    }
    case "refused":
      if (heard.id === asked(sync.state)) {
        return { sync: { ...sync, state: { kind: "failed" }, window: undefined }, send: [] };
      }
      return { sync: heard.id === sync.window ? { ...sync, window: undefined } : sync, send: [] };
    case "run":
      return chat === undefined ? { sync, send: [] } : { sync: { ...sync, chat: stream(chat, heard.run, heard.event) }, send: [] };
  }
}

// Asks for every event back to the oldest waiting prompt the chat doesn't hold yet.
function fetchWaiting(sync: Sync, ref: ChatRef, id: () => number, send: Envelope[]): Step {
  const { chat } = sync;
  const oldest = chat && missing(chat);
  if (chat === undefined || oldest === undefined) {
    return { sync, send };
  }
  const step = windowTo(sync, chat, ref, id, oldest);
  return { sync: step.sync, send: [...send, ...step.send] };
}

function windowTo(sync: Sync, chat: Chat, ref: ChatRef, id: () => number, from: number): Step {
  const query = ask(ref, id(), { kind: "window", before: chat.first, limit: chat.first - from });
  return { sync: { ...sync, window: query.id }, send: [query] };
}

// The envelope id of the snapshot or subscription `state` waits on, if any.
function asked(state: SyncState): number | undefined {
  return state.kind === "asking" ? state.snapshot : state.kind === "live" ? state.subscription : undefined;
}

function ask(ref: ChatRef, id: number, query: ChatQueryKind): Envelope {
  return { id, type: "chat_query", ...ref, query };
}
