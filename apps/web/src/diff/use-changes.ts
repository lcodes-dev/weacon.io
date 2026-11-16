import { useEffect, useState } from "react";

import type { ChatRef } from "../chat-ref.ts";
import { newId } from "../ids.ts";
import { route } from "../incoming.ts";
import type { MachineId } from "../protocol.ts";
import type { WorkspaceSocket } from "../shell/workspace-socket.ts";
import { NO_CHANGES, ask, cut, needsAsking, receive } from "./changes.ts";

/**
 * The changes the agent on `machine` made in the chat at `chatRef`, asked over `link` when they are
 * `shown`, and again when the link comes back while they are, if its answers were cut short;
 * whether they can be asked at all.
 */
export function useChanges(link: WorkspaceSocket, chatRef: ChatRef | undefined, machine: MachineId | undefined, live: boolean, shown: boolean) {
  const [changes, setChanges] = useState(NO_CHANGES);
  useEffect(() => {
    if (!live) {
      return;
    }
    const unsubscribe = link.subscribe((message) =>
      route(message, { changes: (answer) => setChanges((before) => receive(before, answer)) }),
    );
    return () => {
      unsubscribe();
      setChanges(cut);
    };
  }, [link, live]);
  const askable = chatRef !== undefined && machine !== undefined && live;
  useEffect(() => {
    if (shown && askable && needsAsking(changes)) {
      const { query, changes: asked } = ask(link.nextId(), newId(), { ...chatRef, machine }, changes);
      setChanges(asked);
      link.send(query);
    }
    // Only showing them and the link coming back ask; a failed answer waits for one of those.
  }, [shown, askable]);
  return { changes, askable };
}
