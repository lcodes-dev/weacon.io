import { useCallback, useSyncExternalStore } from "react";

import type { WebCapability } from "../connection.ts";
import type { WorkspaceSocket } from "../shell/workspace-socket.ts";

/** Whether a client may read the server's chats, and send to them, given what it offers. */
export function chatAccess(offers: (capability: WebCapability) => boolean): { read: boolean; send: boolean } {
  return { read: offers("workspace.chats"), send: offers("workspace.chat_commands") };
}

/** `chatAccess` on `link`, kept current as welcomes change what the server offers. */
export function useChatAccess(link: WorkspaceSocket): { read: boolean; send: boolean } {
  const subscribe = useCallback((changed: () => void) => link.watch(changed), [link]);
  const read = useSyncExternalStore(subscribe, () => chatAccess((capability) => link.offers(capability)).read);
  const send = useSyncExternalStore(subscribe, () => chatAccess((capability) => link.offers(capability)).send);
  return { read, send };
}
