import { useEffect, useState } from "react";

import type { ChatRef } from "../chat-ref.ts";
import { newRequestId } from "../ids.ts";
import { route } from "../incoming.ts";
import { type WorkspaceSocket, useOffers } from "../shell/workspace-socket.ts";
import { NO_CHANGES, ask, cut, needsAsking, receive } from "./changes.ts";


/**
 * The changes of the chat at `chatRef`, asked over `link` the first time they are opened and again
 * after answers were cut short; whether they can be asked at all, while the link is `live`.
 */
export function useChanges(link: WorkspaceSocket, chatRef: ChatRef | undefined, live: boolean) {
  const [changes, setChanges] = useState(NO_CHANGES);
  const offered = useOffers(link, "diff.view");
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
  const askable = chatRef !== undefined && live && offered;
  const open = () => {
    if (askable && needsAsking(changes)) {
      const { query, changes: asked } = ask(link.nextId(), newRequestId(), chatRef.workspace, chatRef.chat, changes);
      setChanges(asked);
      link.send(query);
    }
  };
  return { changes, askable, open };
}
