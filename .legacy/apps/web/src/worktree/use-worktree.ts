import { useEffect, useState } from "react";

import type { ChatRef } from "../chat-ref.ts";
import { newId } from "../ids.ts";
import { route } from "../incoming.ts";
import type { MachineId } from "../protocol.ts";
import type { WorkspaceSocket } from "../shell/workspace-socket.ts";
import { type Ask, UNASKED, answered, cancelled, ended, envelopeFor, resumeLine, unsent, waiting } from "./worktree.ts";

/**
 * The worktree the chat at `chatRef` has on `machine`, asked over `link` while it is `live`: its
 * state each time it is `shown`, and answers while it isn't. A pull request's draft stays until the
 * core opens it.
 */
export function useWorktree(link: WorkspaceSocket, chatRef: ChatRef | undefined, machine: MachineId | undefined, live: boolean, shown: boolean) {
  const [worktree, setWorktree] = useState(UNASKED);
  const [draft, setDraft] = useState<{ title: string; body: string }>();
  const usable = chatRef !== undefined && machine !== undefined && live;
  // Whether `ask` went out; the screen says so when it couldn't.
  const ask = (what: Ask): boolean => {
    if (chatRef === undefined || machine === undefined) {
      return false;
    }
    const pending = { id: link.nextId(), request: newId(), ask: what.kind, confirm: what.kind === "state" && what.confirm === true };
    const sent = link.send(envelopeFor({ ...chatRef, machine }, pending.id, pending.request, what));
    setWorktree((before) => (sent ? waiting(before, pending) : unsent(before, what.kind)));
    return sent;
  };
  const workspace = chatRef?.workspace;
  const chat = chatRef?.chat;
  useEffect(() => {
    if (!usable || workspace === undefined || chat === undefined) {
      return;
    }
    const ref = { workspace, chat };
    const unsubscribe = link.subscribe((message) => route(message, { worktree: (heard) => setWorktree((before) => answered(before, ref, heard)) }));
    return () => {
      unsubscribe();
      setWorktree(ended);
    };
  }, [link, usable, workspace, chat]);
  const asking = usable && shown;
  useEffect(() => {
    if (asking) {
      ask({ kind: "state" });
    }
  }, [asking]);
  const opened = worktree.outcome?.kind === "link";
  useEffect(() => {
    if (opened) {
      setDraft(undefined);
    }
  }, [opened]);
  return {
    worktree,
    resume: chatRef === undefined ? undefined : resumeLine(chatRef),
    usable,
    refresh: () => ask({ kind: "state" }),
    remove: () => ask({ kind: "state", confirm: true }),
    confirmRemove: () => {
      if (ask({ kind: "remove" })) {
        setWorktree(cancelled);
      }
    },
    cancel: () => setWorktree(cancelled),
    draft,
    editDraft: setDraft,
    openPullRequest: () => draft !== undefined && ask({ kind: "open_pull_request", title: draft.title.trim(), body: draft.body }),
    openTerminal: () => ask({ kind: "open_terminal" }),
  };
}
