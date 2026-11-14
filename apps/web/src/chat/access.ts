import { useSyncExternalStore } from "react";

import type { WebCapability } from "../connection.ts";
import type { WorkspaceSocket } from "../shell/workspace-socket.ts";

// The second code of each pair is the fake server's.
const READS: readonly WebCapability[] = ["workspace.chats", "chat.events"];
const SENDS: readonly WebCapability[] = ["workspace.chat_commands", "chat.send"];

/** Whether a client may read the server's chats, and send to them, given what it offers. */
export function chatAccess(offers: (capability: WebCapability) => boolean): { read: boolean; send: boolean } {
  return { read: READS.some(offers), send: SENDS.some(offers) };
}

/** `chatAccess` on `link`, kept current as welcomes change what the server offers. */
export function useChatAccess(link: WorkspaceSocket): { read: boolean; send: boolean } {
  const subscribe = (changed: () => void) => link.watch(changed);
  const read = useSyncExternalStore(subscribe, () => chatAccess((capability) => link.offers(capability)).read);
  const send = useSyncExternalStore(subscribe, () => chatAccess((capability) => link.offers(capability)).send);
  return { read, send };
}
