import { type Chat, commit, fromSnapshot, missing, older } from "./chat.ts";
import type { Incoming } from "./incoming.ts";
import type { ChatQueryKind, Envelope, WorkspaceId, ChatId } from "./protocol.ts";

/** Which chat. */
export type ChatRef = { workspace: WorkspaceId; chat: ChatId };

/**
 * Where the chat stands with the core, each query by its envelope id: waiting on its snapshot,
 * subscribed (with a window of older events on its way, perhaps), or refused.
 */
type State =
  | { kind: "asking"; snapshot: number }
  | { kind: "live"; subscription: number; window: number | undefined }
  | { kind: "failed"; code: string };

/** The chat shown, and where it stands. */
export type Sync = { ref: ChatRef; chat: Chat | undefined; state: State };

/** The queries to send, and the state they leave. */
export type Step = { sync: Sync; send: Envelope[] };

/** Asks for the chat afresh, keeping what it shows until the answer comes. */
export function open(ref: ChatRef, chat: Chat | undefined, id: () => number): Step {
  const query = ask(ref, id(), { kind: "snapshot" });
  return { sync: { ref, chat, state: { kind: "asking", snapshot: query.id } }, send: [query] };
}

/** What hearing `message` does to `sync`; a stale answer, or one about another chat, does nothing. */
export function hear(sync: Sync, message: Incoming, id: () => number): Step {
  const { ref, chat, state } = sync;
  const same = { sync, send: [] };
  if (message.type === "error") {
    const refused = state.kind === "asking" ? message.id === state.snapshot : state.kind === "live" && message.id === state.subscription;
    if (refused) {
      return { sync: { ...sync, state: { kind: "failed", code: message.code } }, send: [] };
    }
    return state.kind === "live" && message.id === state.window ? { sync: { ...sync, state: { ...state, window: undefined } }, send: [] } : same;
  }
  if ((message.type !== "chat" && message.type !== "chat_answer") || message.workspace !== ref.workspace || message.chat !== ref.chat) {
    return same;
  }
  if (message.type === "chat") {
    if (chat === undefined || state.kind !== "live") {
      return same;
    }
    const next = commit(chat, message.entry);
    return next === undefined ? open(ref, chat, id) : { sync: { ...sync, chat: next }, send: [] };
  }
  const { answer } = message;
  switch (answer.kind) {
    case "events_lost":
      // Pushed, so under no request's id.
      return state.kind === "live" ? open(ref, chat, id) : same;
    case "snapshot": {
      if (state.kind !== "asking" || message.id !== state.snapshot) {
        return same;
      }
      const fresh = fromSnapshot(answer);
      const subscribe = ask(ref, id(), { kind: "subscribe", after: fresh.seq });
      return fetchWaiting({ ref, chat: fresh, state: { kind: "live", subscription: subscribe.id, window: undefined } }, id, [subscribe]);
    }
    case "window": {
      if (chat === undefined || state.kind !== "live" || message.id !== state.window) {
        return same;
      }
      const next = older(chat, answer.events);
      const settled = { ...sync, state: { ...state, window: undefined } };
      return next === chat ? { sync: settled, send: [] } : fetchWaiting({ ...settled, chat: next }, id, []);
    }
  }
}

// Asks for every event back to the oldest waiting prompt the chat doesn't hold yet.
function fetchWaiting(sync: Sync, id: () => number, send: Envelope[]): Step {
  const { chat, state } = sync;
  const oldest = chat === undefined ? undefined : missing(chat);
  if (chat === undefined || oldest === undefined || state.kind !== "live") {
    return { sync, send };
  }
  const query = ask(sync.ref, id(), { kind: "window", before: chat.first, limit: chat.first - oldest });
  return { sync: { ...sync, state: { ...state, window: query.id } }, send: [...send, query] };
}

function ask(ref: ChatRef, id: number, query: ChatQueryKind): Envelope & { id: number } {
  return { id, type: "chat_query", ...ref, query };
}
