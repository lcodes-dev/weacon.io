import { useEffect, useRef, useState } from "react";

import type { ChatRef } from "../chat-ref.ts";
import { type Send, deliverPrompt } from "../composer/prompt.ts";
import { later } from "../connection.ts";
import type { Choice } from "../new-chat/start.ts";
import { newId } from "../ids.ts";
import { route } from "../incoming.ts";
import type { ChatAction, Envelope, RequestId, RunId } from "../protocol.ts";
import { Receipts } from "../receipts.ts";
import type { WorkspaceSocket } from "../shell/workspace-socket.ts";
import { type Answers, askKey, sending, settle, stopProblem } from "./asks.ts";
import { type Command, chatCommand, deliver } from "./commands.ts";
import { receiptsIn } from "./heard.ts";
import { ChatFeeds } from "./chat-feeds.ts";
import { type LiveChat, nextFrame } from "./live-chat.ts";
import { START, type Sync, ended } from "./sync.ts";

// How long a hidden tab waits for a frame before its batch of run events goes out anyway.
const HIDDEN_MS = 250;

// Each socket's chats, kept for as long as the socket is.
const FEEDS = new WeakMap<WorkspaceSocket, ChatFeeds>();

function feedsOf(link: WorkspaceSocket): ChatFeeds {
  const known = FEEDS.get(link);
  if (known !== undefined) {
    return known;
  }
  const schedule = () =>
    nextFrame(
      () => document.hidden,
      (run) => requestAnimationFrame(run),
      (frame) => cancelAnimationFrame(frame),
      (run) => later(run, HIDDEN_MS),
    );
  const made = new ChatFeeds(link, schedule);
  FEEDS.set(link, made);
  return made;
}

/**
 * The chat at `chatRef`, kept in step over `link` while it is `live` and the server lets this
 * client read it; what it shows stays once that ends.
 */
export function useLiveChat(link: WorkspaceSocket, chatRef: ChatRef | undefined, live: boolean, readable: boolean) {
  const [sync, setSync] = useState<Sync>(START);
  const current = useRef<LiveChat>(undefined);
  const workspace = chatRef?.workspace;
  const chat = chatRef?.chat;
  useEffect(() => {
    if (workspace === undefined || chat === undefined || !live || !readable) {
      return;
    }
    const { chat: liveChat, hide } = feedsOf(link).show({ workspace, chat }, setSync);
    current.current = liveChat;
    return () => {
      hide();
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
 * Messages, prompts, permission answers and Stop for the chat at `chatRef` over `link`, and the
 * first words that make a chat, each waiting for its receipt while the link is `live`; an accepted
 * answer tells `answered` its prompt.
 */
export function useChatCommands(link: WorkspaceSocket, chatRef: ChatRef | undefined, live: boolean, answered: (run: RunId, prompt: number) => void) {
  const [answers, setAnswers] = useState<Answers>(new Map());
  const [stopping, setStopping] = useState(false);
  const [stopFailed, setStopFailed] = useState<string>();
  const receipts = useRef<Receipts>(undefined);
  useEffect(() => {
    if (!live) {
      return;
    }
    const waiting = new Receipts(later);
    receipts.current = waiting;
    const unsubscribe = link.subscribe((message) =>
      route(message, {
        chat: (said) => receiptsIn(said).forEach((receipt) => waiting.take(receipt)),
        answers: (answer) => waiting.take(answer),
      }),
    );
    return () => {
      unsubscribe();
      waiting.end();
      receipts.current = undefined;
    };
  }, [link, live]);
  const command = <T,>(action: ChatAction, request: RequestId, through: Through<T>, to = chatRef): Promise<T | "lost"> => {
    const waiting = receipts.current;
    if (waiting === undefined || to === undefined) {
      return Promise.resolve("lost");
    }
    const send = (envelope: Envelope) => {
      if (!link.send(envelope)) {
        throw new Error("the feed isn't live");
      }
    };
    return through(chatCommand(link.nextId(), to, request, action), waiting, send);
  };
  // A message to the chat's people, or, when it calls the sender's agent, a prompt to it, which
  // names the agent `first` picks until the chat has it.
  const send =
    (first?: Choice): Send =>
    (text, request, calls) =>
      command(calls ? { kind: "prompt", text, ...first } : { kind: "say", text }, request, deliverPrompt);
  // A chat's first words, which make the chat at `to`.
  const start = (to: ChatRef, action: Extract<ChatAction, { kind: "say" | "prompt" }>, request: RequestId) => command(action, request, deliverPrompt, to);
  const act = (action: ChatAction, request: RequestId) => command(action, request, deliver);
  const answering = {
    answers,
    answer(run: RunId, prompt: number, allow: boolean) {
      const request = newId();
      setAnswers((before) => sending(before, { run, prompt, allow }, request));
      void act({ kind: "permission", answer: { run, prompt, allow } }, request).then((settled) => {
        if (settled === "accepted") {
          answered(run, prompt);
        }
        setAnswers((before) => settle(before, askKey(run, prompt), request, settled));
      });
    },
  };
  const stop = () => {
    setStopping(true);
    setStopFailed(undefined);
    void act({ kind: "interrupt" }, newId()).then((settled) => {
      setStopping(false);
      setStopFailed(stopProblem(settled));
    });
  };
  return { send, start, answering, stop: { stopping, problem: stopFailed, stop } };
}
