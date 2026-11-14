import { useEffect, useRef, useState } from "react";

import { chatRefOf } from "../chat-ref.ts";
import { type Answers, askKey, sending, settle, settleLate, stopProblem } from "../chat/asks.ts";
import { working } from "../chat/chat.ts";
import { receiptsIn } from "../chat/heard.ts";
import { lines } from "../chat/lines.ts";
import { LiveChat, nextFrame } from "../chat/live-chat.ts";
import { START, type Sync } from "../chat/sync.ts";
import { type Late, type Send, type Sent, chatCommand, deliver } from "../composer/prompt.ts";
import { type Asks, Connection, HELLO_ID, type Later, type Status, offers } from "../connection.ts";
import { NO_CHANGES, ask, cut, needsAsking, receive } from "../diff/changes.ts";
import { newRequestId } from "../ids.ts";
import { route } from "../incoming.ts";
import type { Log } from "../log.ts";
import { ME } from "../me.ts";
import type { ChatAction, RequestId, RunId } from "../protocol.ts";
import { Receipts } from "../receipts.ts";
import { ChatPageView } from "./chat-page-view.tsx";

const chatRef = chatRefOf(window.location.search);
// How long a hidden tab waits for a frame before its batch of run events goes out anyway.
const HIDDEN_MS = 250;
const TRANSCRIPT: Asks = { needs: "run.stream", offers: ["diff.view", "chat.events", "log.forward"] };
// No message carries members' names, so labels leave them out.
const NO_MEMBERS = new Map<string, string>();

/** A chat served by the fake server, with its own connection, for development and the end-to-end tests. */
export function ChatPage({ url, log }: { url: string; log: Log }) {
  const [status, setStatus] = useState<Status>({ state: "connecting" });
  const [sync, setSync] = useState<Sync>(START);
  const [answers, setAnswers] = useState<Answers>(new Map());
  // Every answer's request, with its prompt, for a late receipt to close the prompt it answers,
  // even after a later answer to the same prompt replaced it on screen.
  const answered = useRef(new Map<RequestId, { run: RunId; prompt: number }>());
  const [stopping, setStopping] = useState(false);
  const [stopFailed, setStopFailed] = useState<string>();
  const [late, setLate] = useState<Late>();
  const [changes, setChanges] = useState(NO_CHANGES);
  // The connection while it is live, and the commands sent on it that wait for an answer.
  const live = useRef<{ connection: Connection; receipts: Receipts; liveChat: LiveChat | undefined }>(undefined);
  const lastId = useRef(HELLO_ID);
  useEffect(() => {
    let socket: WebSocket;
    try {
      socket = new WebSocket(url);
    } catch {
      // Only a malformed `?server=` gets here.
      setStatus({ state: "ended", code: "connection.refused" });
      return;
    }
    const later: Later = (run, ms) => setTimeout(run, ms);
    const receipts = new Receipts(later, (request, settled) => {
      setLate({ request, settled });
      const answer = answered.current.get(request);
      if (answer !== undefined && settled === "accepted") {
        live.current?.liveChat?.answered(answer.run, answer.prompt);
      }
      setAnswers((before) => settleLate(before, request, settled));
    });
    const schedule = nextFrame(
      () => document.hidden,
      (run) => requestAnimationFrame(run),
      (frame) => cancelAnimationFrame(frame),
      (run) => later(run, HIDDEN_MS),
    );
    // What ends with the connection, however it ends.
    const stop = () => {
      live.current = undefined;
      log.detach();
      receipts.end();
    };
    const connection = new Connection(socket, ME, {
      status(next) {
        setStatus(next);
        if (next.state === "live") {
          const liveChat =
            chatRef === undefined || !offers(next, "chat.events")
              ? undefined
              : new LiveChat(chatRef, {
                  id: () => ++lastId.current,
                  send: (envelope) => connection.send(envelope),
                  schedule,
                  show: setSync,
                });
          live.current = { connection, receipts, liveChat };
          liveChat?.open();
          log.attach((envelope) => connection.send(envelope), offers(next, "log.forward"));
        } else if (next.state === "ended") {
          live.current?.liveChat?.end();
          stop();
          setChanges(cut);
          log.warn(next.code === "connection.lost" ? "connection lost" : "connection refused", { code: next.code });
        }
      },
      message(envelope) {
        route(envelope, {
          run: (message) => live.current?.liveChat?.push(message),
          chat: (message) => {
            for (const receipt of receiptsIn(message)) {
              receipts.take(receipt);
            }
            live.current?.liveChat?.push(message);
          },
          answers: (message) => receipts.take(message),
          changes: (message) => setChanges((before) => receive(before, message)),
        });
      },
    }, TRANSCRIPT, later);
    return () => {
      live.current?.liveChat?.close();
      stop();
      connection.close();
    };
  }, [url]);
  const command = (action: ChatAction, request: RequestId): Promise<Sent> => {
    const now = live.current;
    if (now === undefined || chatRef === undefined) {
      return Promise.resolve("lost");
    }
    return deliver(chatCommand(++lastId.current, chatRef, request, action), now.receipts, (envelope) => now.connection.send(envelope));
  };
  const send: Send = (text, request) => command({ kind: "prompt", text }, request);
  const answering = {
    answers,
    answer(run: RunId, prompt: number, allow: boolean) {
      const key = askKey(run, prompt);
      const { answers: next, request } = sending(answers, { run, prompt, allow }, newRequestId);
      answered.current.set(request, { run, prompt });
      setAnswers(next);
      void command({ kind: "permission", answer: { run, prompt, allow } }, request).then((sent) => {
        // An answer is far too short to be refused for its size.
        const settled = sent === "too_long" ? "refused" : sent;
        setAnswers((before) => settle(before, key, request, settled));
        if (settled === "accepted") {
          live.current?.liveChat?.answered(run, prompt);
        }
      });
    },
  };
  const stop = () => {
    setStopping(true);
    setStopFailed(undefined);
    void command({ kind: "interrupt" }, newRequestId()).then((sent) => {
      setStopping(false);
      setStopFailed(stopProblem(sent));
    });
  };
  const { chat } = sync;
  const shown = {
    lines: chat === undefined ? [] : lines(chat, NO_MEMBERS),
    older: chat !== undefined && sync.live && chat.first > 1 ? () => live.current?.liveChat?.more() : undefined,
    answering,
    // A chat is on its way while the connection is made, then only from a server that serves chats.
    loading: chatRef !== undefined && chat === undefined && !sync.failed && (status.state === "connecting" || offers(status, "chat.events")),
    retry: sync.failed ? () => live.current?.liveChat?.open() : undefined,
    stop: chat !== undefined && working(chat) ? { stopping, problem: stopFailed, stop } : undefined,
  };
  const openChanges = () => {
    // Asked the first time they are opened, and again after answers were cut short.
    if (needsAsking(changes) && chatRef !== undefined && live.current !== undefined) {
      const { query, changes: asked } = ask(++lastId.current, newRequestId(), chatRef.workspace, chatRef.chat, changes);
      setChanges(asked);
      live.current.connection.send(query);
    }
  };
  return (
    <ChatPageView me={ME} status={status} chat={shown} chatRef={chatRef} send={send} late={late} changes={changes} openChanges={openChanges} />
  );
}
