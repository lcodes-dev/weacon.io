import type { ChatId, WorkspaceId } from "./protocol.ts";

/** A chat, and the workspace it is in. */
export type Place = { workspace: WorkspaceId; chat: ChatId };

/** The chat that `?workspace=` and `?chat=` name, when both are well-formed ids. */
export function placeOf(search: string): Place | undefined {
  const query = new URLSearchParams(search);
  const workspace = query.get("workspace");
  const chat = query.get("chat");
  return workspace !== null && chat !== null && isId(workspace) && isId(chat) ? { workspace, chat } : undefined;
}

/** Whether `text` has the form of the protocol's ids (`token_id!`): 1–64 ASCII letters, digits, `-` or `_`. */
export function isId(text: string): boolean {
  return /^[A-Za-z0-9_-]{1,64}$/.test(text);
}
