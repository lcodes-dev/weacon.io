import { useEffect, useRef, useState } from "react";

import type { ChatRef } from "../chat-ref.ts";
import { type Late, type Send, deliverPrompt } from "../composer/prompt.ts";
import type { Later } from "../connection.ts";
import { newRequestId } from "../ids.ts";
import { route } from "../incoming.ts";
import type { ChatAction, Envelope, RequestId, RunId } from "../protocol.ts";
import { Receipts, type Settled } from "../receipts.ts";
import type { Link } from "../shell/workspace-socket.ts";
import { type Answers, sending, settleAnswer, stopProblem } from "./asks.ts";
import { type Command, chatCommand, deliver } from "./commands.ts";
import { receiptsIn } from "./heard.ts";
import { LiveChat, nextFrame } from "./live-chat.ts";
import { START, type Sync, ended } from "./sync.ts";

// How long a hidden tab waits for a frame before its batch of run events goes out anyway.
const HIDDEN_MS = 250;
const later: Later = (run, ms) => setTimeout(run, ms);
const ignore = () => {};

/**
 * The chat at `chatRef`, kept in step over `link` while it is `live` and the server lets this
 * client read it; what it shows stays once that ends.
 */
export function useLiveChat(link: Link, chatRef: ChatRef | undefined, live: boolean, readable: boolean) {
  const [sync, setSync] = useState<Sync>(START);
  const current = useRef<LiveChat>(undefined);
  const workspace = chatRef?.workspace;
  const chat = chatRef?.chat;
  useEffect(() => {
    if (workspace === undefined || chat === undefined || !live || !readable) {
      return;
    }
    const schedule = nextFrame(
      () => document.hidden,
      (run) => requestAnimationFrame(run),
      (frame) => cancelAnimationFrame(frame),
      (run) => later(run, HIDDEN_MS),
    );
    const liveChat = new LiveChat({ workspace, chat }, { id: () => link.nextId(), send: (envelope) => link.send(envelope), schedule, show: setSync });
    current.current = liveChat;
    const unsubscribe = link.subscribe((message) =>
      route(message, { run: (run) => liveChat.push(run), chat: (said) => liveChat.push(said), answers: ignore, changes: ignore }),
    );
    liveChat.open();
    return () => {
      unsubscribe();
      liveChat.close();
      current.current = undefined;
      setSync(ended);
    };
  }, [link, workspace, chat, live, readable]);
  return {
    sync,
    more: () => current.current?.more(),
    open: () => current.current?.open(),
    answered: (run: RunId, prompt: number) => current.current?.answered(run, prompt),
  };
}

type Through<T> = (command: Command, receipts: Receipts, send: (envelope: Envelope) => void) => Promise<T>;

/**
 * Prompts, permission answers and Stop for the chat at `chatRef` over `link`, each waiting for its
 * receipt while the link is `live`; an accepted answer tells `answered` its prompt.
 */
export function useChatCommands(link: Link, chatRef: ChatRef | undefined, live: boolean, answered: (run: RunId, prompt: number) => void) {
  const [answers, setAnswers] = useState<Answers>(new Map());
  // Every answer's request, with its prompt, for a late receipt to close the prompt it answers,
  // even after a later answer to the same prompt replaced it on screen.
  const sent = useRef(new Map<RequestId, { run: RunId; prompt: number }>());
  const [stopping, setStopping] = useState(false);
  const [stopFailed, setStopFailed] = useState<string>();
  const [late, setLate] = useState<Late>();
  const receipts = useRef<Receipts>(undefined);
  const answerSettled = (request: RequestId, settled: Settled) => {
    const answer = sent.current.get(request);
    if (answer !== undefined && settled === "accepted") {
      answered(answer.run, answer.prompt);
    }
    setAnswers((before) => settleAnswer(before, sent.current, request, settled));
  };
  useEffect(() => {
    if (!live) {
      return;
    }
    const waiting = new Receipts(later, (request, settled) => {
      setLate({ request, settled });
      answerSettled(request, settled);
    });
    receipts.current = waiting;
    const unsubscribe = link.subscribe((message) =>
      route(message, {
        run: ignore,
        chat: (said) => receiptsIn(said).forEach((receipt) => waiting.take(receipt)),
        answers: (answer) => waiting.take(answer),
        changes: ignore,
      }),
    );
    return () => {
      unsubscribe();
      waiting.end();
      receipts.current = undefined;
    };
  }, [link, live]);
  const command = <T,>(action: ChatAction, request: RequestId, through: Through<T>): Promise<T | "lost"> => {
    const waiting = receipts.current;
    if (waiting === undefined || chatRef === undefined) {
      return Promise.resolve("lost");
    }
    const send = (envelope: Envelope) => {
      if (!link.send(envelope)) {
        throw new Error("the feed isn't live");
      }
    };
    return through(chatCommand(link.nextId(), chatRef, request, action), waiting, send);
  };
  const send: Send = (text, request) => command({ kind: "prompt", text }, request, deliverPrompt);
  const act = (action: ChatAction, request: RequestId) => command(action, request, deliver);
  const answering = {
    answers,
    answer(run: RunId, prompt: number, allow: boolean) {
      const { answers: next, request } = sending(answers, { run, prompt, allow }, newRequestId);
      sent.current.set(request, { run, prompt });
      setAnswers(next);
      void act({ kind: "permission", answer: { run, prompt, allow } }, request).then((settled) => answerSettled(request, settled));
    },
  };
  const stop = () => {
    setStopping(true);
    setStopFailed(undefined);
    void act({ kind: "interrupt" }, newRequestId()).then((settled) => {
      setStopping(false);
      setStopFailed(stopProblem(settled));
    });
  };
  return { send, late, answering, stop: { stopping, problem: stopFailed, stop } };
}
