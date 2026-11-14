import { isId } from "./ids.ts";
import type { ChatId, WorkspaceId } from "./protocol.ts";

/** A chat, and the workspace it is in. */
export type ChatRef = { workspace: WorkspaceId; chat: ChatId };

/** The chat that `?workspace=` and `?chat=` name, when both are well-formed ids. */
export function chatRefOf(search: string): ChatRef | undefined {
  const query = new URLSearchParams(search);
  const workspace = query.get("workspace");
  const chat = query.get("chat");
  return workspace !== null && chat !== null && isId(workspace) && isId(chat) ? { workspace, chat } : undefined;
}
