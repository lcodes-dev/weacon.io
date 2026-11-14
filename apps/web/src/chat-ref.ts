import { isId } from "./ids.ts";
import type { ChatId, WorkspaceId } from "./protocol.ts";

/** A chat, and the workspace it is in. */
export type ChatRef = { workspace: WorkspaceId; chat: ChatId };

/** The chat that `?workspace=` and `?chat=` name, when both are well-formed ids. */
export function chatRefOf(search: string): ChatRef | undefined {
  const workspace = workspaceOf(search);
  const chat = new URLSearchParams(search).get("chat");
  return workspace !== undefined && chat !== null && isId(chat) ? { workspace, chat } : undefined;
}

/** The workspace `?workspace=` names, when it is a well-formed id. */
export function workspaceOf(search: string): WorkspaceId | undefined {
  const workspace = new URLSearchParams(search).get("workspace");
  return workspace !== null && isId(workspace) ? workspace : undefined;
}
