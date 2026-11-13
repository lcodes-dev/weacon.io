import type { ChatId, WorkspaceId } from "./protocol.ts";

/** A chat, and the workspace it is in. */
export type Place = { workspace: WorkspaceId; chat: ChatId };

/** The chat that `?workspace=` and `?chat=` name, when both are well-formed ids. */
export function placeOf(search: string): Place | undefined {
  const workspace = workspaceOf(search);
  const chat = new URLSearchParams(search).get("chat");
  return workspace !== undefined && chat !== null && isId(chat) ? { workspace, chat } : undefined;
}

/** The workspace `?workspace=` names, when it is a well-formed id. */
export function workspaceOf(search: string): WorkspaceId | undefined {
  const workspace = new URLSearchParams(search).get("workspace");
  return workspace !== null && isId(workspace) ? workspace : undefined;
}

/** Whether `text` has the form of the protocol's ids (`token_id!`): 1–64 ASCII letters, digits, `-` or `_`. */
export function isId(text: string): boolean {
  return /^[A-Za-z0-9_-]{1,64}$/.test(text);
}
