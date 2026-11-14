import type { ChatState, ChatSummary } from "../protocol.ts";

/** `chats`, the one whose latest turn began or ended last first. */
export function newestFirst(chats: readonly ChatSummary[]): ChatSummary[] {
  return [...chats].sort((a, b) => b.updated_at - a.updated_at || a.id.localeCompare(b.id));
}

const NOTES: Partial<Record<ChatState, string>> = { full: "Full", detached: "Read only" };

/** What a chat's row says about why it takes no prompts, if it doesn't. */
export function stateNote(state: ChatState): string | undefined {
  return NOTES[state];
}
