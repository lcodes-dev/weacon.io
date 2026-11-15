import { useCallback, useEffect, useRef, useState } from "react";

import { ME } from "./build.ts";
import { Failure, HOST, type Link, connect, report } from "./core.ts";
import { reported } from "./feedback.ts";
import type { Incoming } from "./incoming.ts";
import type { ChatAction, Envelope, LocalChat, Pick } from "./protocol.ts";
import { type ChatRef, type Sync, hear, open } from "./sync.ts";

/** Where the panel stands with the core. */
export type Status = { kind: "connecting" } | { kind: "ready" } | { kind: "failed"; code: string };

/** The panel's view of the core: its chats, the chat `ref` names, and what can be asked of it. */
export type Live = {
  status: Status;
  chats: readonly LocalChat[] | undefined;
  sync: Sync | undefined;
  /** The last command's failure, until the next command. */
  refused: string | undefined;
  reconnect(): void;
  /** Sends `action` to the chat shown; a prompt's `pick` is kept by the core first. */
  command(action: ChatAction, pick?: Pick): Promise<void>;
};

/** A code for `error`, which is a `Failure` when the panel knows what went wrong. */
export function codeOf(error: unknown): string {
  return error instanceof Failure ? error.code : "extension.failed";
}

/** Tells the core about `code`, if it is the extension's own. */
export function tell(code: string): void {
  if (reported(code)) {
    void report(chrome.runtime.connectNative(HOST), ME, code);
  }
}

/** A conversation with the core for as long as `ref` is the chat shown; a new chat starts a new one. */
export function useLive(ref: ChatRef | undefined): Live {
  const [status, setStatus] = useState<Status>({ kind: "connecting" });
  const [chats, setChats] = useState<readonly LocalChat[]>();
  const [sync, setSync] = useState<Sync>();
  const [refused, setRefused] = useState<string>();
  const [attempt, setAttempt] = useState(0);
  const link = useRef<Link | undefined>(undefined);
  const commands = useRef(new Set<number>());
  // Kept across connections, so a reconnect shows the chat until its snapshot replaces it.
  const shown = useRef<Sync | undefined>(undefined);

  useEffect(() => {
    let current = true;
    const port = chrome.runtime.connectNative(HOST);
    const same = shown.current?.ref.workspace === ref?.workspace && shown.current?.ref.chat === ref?.chat;
    const kept = same ? shown.current?.chat : undefined;
    if (!same) {
      shown.current = undefined;
      setSync(undefined);
    }
    const apply = (step: { sync: Sync; send: Envelope[] }, to: Link) => {
      shown.current = step.sync;
      setSync(step.sync);
      step.send.forEach((envelope) => to.send(envelope));
    };
    const heard = (message: Incoming) => {
      if (!current || link.current === undefined) {
        return;
      }
      if (message.type === "error" && commands.current.delete(message.id)) {
        setRefused(message.code);
      }
      if (shown.current !== undefined) {
        apply(hear(shown.current, message, link.current.next), link.current);
      }
    };
    const closed = () => current && setStatus({ kind: "failed", code: "extension.core_unreachable" });
    setStatus({ kind: "connecting" });
    connect(port, ME, heard, closed)
      .then(async (opened) => {
        if (!current) {
          return opened.close();
        }
        link.current = opened;
        setStatus({ kind: "ready" });
        if (ref !== undefined) {
          apply(open(ref, kept, opened.next), opened);
        }
        const answer = await opened.ask({ id: opened.next(), type: "chats_query", query: { kind: "list" } });
        if (current && answer.type === "chats_answer" && answer.answer.kind === "list") {
          setChats(answer.answer.chats);
        }
      })
      .catch((error: unknown) => {
        const code = codeOf(error);
        tell(code);
        return current && setStatus({ kind: "failed", code });
      });
    return () => {
      current = false;
      link.current?.close();
      link.current = undefined;
    };
  }, [ref?.workspace, ref?.chat, attempt]);

  const command = useCallback(
    async (action: ChatAction, pick?: Pick) => {
      const to = link.current;
      if (to === undefined || ref === undefined) {
        throw new Failure("extension.core_unreachable");
      }
      setRefused(undefined);
      if (pick !== undefined) {
        const kept = await to.ask({ id: to.next(), type: "pick", ...pick });
        const failed = kept.type === "error" ? kept.code : kept.type === "receipt" && kept.state === "failed" ? kept.code : undefined;
        if (failed !== undefined || kept.type !== "receipt") {
          throw new Failure(failed ?? "extension.unexpected_reply");
        }
      }
      const id = to.next();
      commands.current.add(id);
      to.send({ id, type: "chat_command", request: crypto.randomUUID(), ...ref, action });
    },
    [ref?.workspace, ref?.chat],
  );

  return { status, chats, sync, refused, reconnect: () => setAttempt((at) => at + 1), command };
}
