import type { Place } from "../place.ts";
import type { ChatQueryKind, Envelope } from "../protocol.ts";
import { type Chat, commit, fromSnapshot, missing, older, stream } from "./chat.ts";
import type { Heard } from "./parse.ts";

/**
 * Where a chat's screen stands with the server: the chat it shows, the snapshot and window it
 * waits for, by envelope id, and whether it is subscribed. A new snapshot replaces the chat only
 * once it arrives, and an answer to a query that was given up on is dropped.
 */
export type Sync = { chat: Chat | undefined; snapshot: number | undefined; window: number | undefined; live: boolean };

/** The queries to send, and the state they leave. */
export type Step = { sync: Sync; send: Envelope[] };

export const START: Sync = { chat: undefined, snapshot: undefined, window: undefined, live: false };

// How many older events scrolling up asks for at a time.
const PAGE = 100;

/** Asks for the chat afresh, keeping what it shows until the answer comes, as on opening it. */
export function open(sync: Sync, place: Place, id: () => number): Step {
  const query = ask(place, id(), { kind: "snapshot" });
  return { sync: { ...sync, snapshot: query.id, window: undefined, live: false }, send: [query] };
}

/** Stops waiting for answers, keeping the chat, as when the connection ends. */
export function ended(sync: Sync): Sync {
  return { ...sync, snapshot: undefined, window: undefined, live: false };
}

/** Asks for the page of events before the oldest the chat holds, unless one is on its way. */
export function more(sync: Sync, place: Place, id: () => number): Step {
  const { chat } = sync;
  if (chat === undefined || !sync.live || sync.window !== undefined || chat.first <= 1) {
    return { sync, send: [] };
  }
  return windowTo(sync, chat, place, id, Math.max(1, chat.first - PAGE));
}

/** What hearing `heard` does to `sync`. */
export function hear(sync: Sync, heard: Heard, place: Place, id: () => number): Step {
  const { chat } = sync;
  switch (heard.kind) {
    case "snapshot": {
      if (heard.id !== sync.snapshot) {
        return { sync, send: [] };
      }
      const fresh = fromSnapshot(heard.snapshot);
      const subscribe = ask(place, id(), { kind: "subscribe", after: fresh.seq });
      const next = { chat: fresh, snapshot: undefined, window: undefined, live: true };
      return fetchWaiting(next, place, id, [subscribe]);
    }
    case "entry": {
      if (chat === undefined || !sync.live) {
        return { sync, send: [] };
      }
      const next = commit(chat, heard.entry);
      return next === undefined ? open(sync, place, id) : { sync: { ...sync, chat: next }, send: [] };
    }
    case "lost":
      return sync.live ? open(sync, place, id) : { sync, send: [] };
    case "window": {
      if (chat === undefined || heard.id !== sync.window) {
        return { sync, send: [] };
      }
      const next = { ...sync, chat: older(chat, heard.events), window: undefined };
      // An empty answer can't bring the oldest waiting prompt any closer.
      return heard.events.length === 0 ? { sync: next, send: [] } : fetchWaiting(next, place, id, []);
    }
    case "run":
      return chat === undefined ? { sync, send: [] } : { sync: { ...sync, chat: stream(chat, heard.run, heard.event) }, send: [] };
  }
}

// Asks for every event back to the oldest waiting prompt the chat doesn't hold yet.
function fetchWaiting(sync: Sync, place: Place, id: () => number, send: Envelope[]): Step {
  const { chat } = sync;
  const oldest = chat === undefined ? undefined : missing(chat);
  if (chat === undefined || oldest === undefined) {
    return { sync, send };
  }
  const step = windowTo(sync, chat, place, id, oldest);
  return { sync: step.sync, send: [...send, ...step.send] };
}

function windowTo(sync: Sync, chat: Chat, place: Place, id: () => number, from: number): Step {
  const query = ask(place, id(), { kind: "window", before: chat.first, limit: chat.first - from });
  return { sync: { ...sync, window: query.id }, send: [query] };
}

function ask(place: Place, id: number, query: ChatQueryKind): Envelope {
  return { id, type: "chat_query", ...place, query };
}
