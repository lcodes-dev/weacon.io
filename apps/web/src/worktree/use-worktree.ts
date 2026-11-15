import { useEffect, useState } from "react";

import type { ChatRef } from "../chat-ref.ts";
import { newRequestId } from "../ids.ts";
import { route } from "../incoming.ts";
import { type WorkspaceSocket, useOffers } from "../shell/workspace-socket.ts";
import { type Ask, UNASKED, answered, cancelled, ended, envelopeFor, unsent, waiting } from "./worktree.ts";

/**
 * The worktree of the chat at `chatRef`, asked over `link` while it is `live`, for its owner (`mine`)
 * alone, each time it is `shown`; what the core offers decides whether there is one, and whether it
 * opens terminals.
 */
export function useWorktree(link: WorkspaceSocket, chatRef: ChatRef | undefined, live: boolean, mine: boolean, shown: boolean) {
  const [worktree, setWorktree] = useState(UNASKED);
  const manages = useOffers(link, "worktree.manage");
  const terminals = useOffers(link, "worktree.open_terminal");
  const usable = chatRef !== undefined && live && mine && manages;
  // Whether `ask` went out; the screen says so when it couldn't.
  const ask = (what: Ask): boolean => {
    if (chatRef === undefined) {
      return false;
    }
    const pending = { id: link.nextId(), request: newRequestId(), ask: what.kind, confirm: what.kind === "state" && what.confirm === true };
    const sent = link.send(envelopeFor(chatRef, pending.id, pending.request, what));
    setWorktree((before) => (sent ? waiting(before, pending) : unsent(before, what.kind)));
    return sent;
  };
  const workspace = chatRef?.workspace;
  const chat = chatRef?.chat;
  const asking = usable && shown;
  useEffect(() => {
    if (!asking || workspace === undefined || chat === undefined) {
      return;
    }
    const ref = { workspace, chat };
    const unsubscribe = link.subscribe((message) => route(message, { worktree: (heard) => setWorktree((before) => answered(before, ref, heard)) }));
    ask({ kind: "state" });
    return () => {
      unsubscribe();
      setWorktree(ended);
    };
  }, [link, asking, workspace, chat]);
  return {
    worktree,
    terminals,
    usable,
    refresh: () => ask({ kind: "state" }),
    remove: () => ask({ kind: "state", confirm: true }),
    confirmRemove: () => {
      if (ask({ kind: "remove" })) {
        setWorktree(cancelled);
      }
    },
    cancel: () => setWorktree(cancelled),
    restore: () => ask({ kind: "restore" }),
    openPullRequest: (title: string, body: string) => ask({ kind: "open_pull_request", title, body }),
    openTerminal: () => ask({ kind: "open_terminal" }),
  };
}
