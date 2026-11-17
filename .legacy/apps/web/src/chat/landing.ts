import type { Chat } from "./chat.ts";
import { lineAt } from "./lines.ts";

/** What opening a chat at a message of its log needs: its older events first, then the line to show; nothing without one. */
export type Landing = { kind: "none" } | { kind: "older" } | { kind: "line"; key: string };

/** How to open `chat` at the message at `at` in its log; nothing once the log has no such message. */
export function landing(chat: Chat | undefined, at: number | undefined): Landing {
  if (chat === undefined || at === undefined || at > chat.seq) {
    return { kind: "none" };
  }
  if (at < chat.first) {
    return chat.first > 1 ? { kind: "older" } : { kind: "none" };
  }
  const key = lineAt(chat, at);
  return key === undefined ? { kind: "none" } : { kind: "line", key };
}
