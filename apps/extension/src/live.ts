import { useCallback, useEffect, useRef, useState } from "react";

import { ME } from "./build.ts";
import { Failure, HOST, type Link, codeOf, connect } from "./core.ts";
import { fitsFrame } from "./fit.ts";
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
  /** Sends `action` to the chat shown. */
  command(action: ChatAction): Promise<void>;
  /** Sends `text` to the chat shown, with `pick`, which the core keeps first. */
  prompt(text: string, pick: Pick | undefined): Promise<void>;
};

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
    // Envelope ids start again with each connection.
    commands.current = new Set();
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
        if (!current) {
          return;
        }
        if (answer.type === "chats_answer" && answer.answer.kind === "list") {
          setChats(answer.answer.chats);
        } else {
          setStatus({ kind: "failed", code: answer.type === "error" ? answer.code : "extension.unexpected_reply" });
        }
      })
      .catch((error: unknown) => {
        const code = codeOf(error);
        return current && setStatus({ kind: "failed", code });
      });
    return () => {
      current = false;
      link.current?.close();
      link.current = undefined;
    };
  }, [ref?.workspace, ref?.chat, attempt]);

  const command = useCallback(
    async (action: ChatAction) => {
      const to = link.current;
      if (to === undefined || ref === undefined) {
        throw new Failure("extension.core_unreachable");
      }
      setRefused(undefined);
      const id = to.next();
      const envelope: Envelope = { id, type: "chat_command", request: crypto.randomUUID(), ...ref, action };
      if (!fitsFrame(envelope)) {
        throw new Failure("extension.message_too_large");
      }
      commands.current.add(id);
      to.send(envelope);
    },
    [ref?.workspace, ref?.chat],
  );
  const prompt = useCallback(
    async (text: string, pick: Pick | undefined) => {
      const to = link.current;
      if (pick === undefined || to === undefined) {
        return command({ kind: "prompt", text });
      }
      const kept = await to.ask({ id: to.next(), type: "pick", ...pick });
      if (kept.type === "error" || (kept.type === "receipt" && kept.state === "failed")) {
        throw new Failure(kept.code);
      }
      if (kept.type !== "receipt") {
        throw new Failure("extension.unexpected_reply");
      }
      return command({ kind: "prompt", text, pick: pick.request });
    },
    [command],
  );

  return { status, chats, sync, refused, reconnect: () => setAttempt((at) => at + 1), command, prompt };
}
