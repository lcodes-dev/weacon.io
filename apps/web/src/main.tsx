import { StrictMode, useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";

import "./app.css";
import { App } from "./app.tsx";
import { chatRefOf } from "./chat-ref.ts";
import { type Late, type Send, type Sent, chatCommand, deliver } from "./composer/prompt.ts";
import { type Asks, Connection, type Later, type Status, offers } from "./connection.ts";
import { NO_CHANGES, ask, cut, needsAsking, receive } from "./diff/changes.ts";
import { type Answers, answeredUnder, askKey, sending, settle, settleLate } from "./chat/asks.ts";
import { working } from "./chat/chat.ts";
import { ChatFeed, nextFrame } from "./chat/feed.ts";
import { lines } from "./chat/lines.ts";
import { START, type Sync } from "./chat/sync.ts";
import { newRequestId } from "./ids.ts";
import { route } from "./incoming.ts";
import { Log } from "./log.ts";
import { ME } from "./me.ts";
import type { ChatAction, RequestId, RunId } from "./protocol.ts";
import { Receipts } from "./receipts.ts";
import { fakeServerUrl } from "./server.ts";
import { Shell } from "./shell/shell.tsx";

const log = new Log();
// A crash is reported by a fixed code, as its message and stack could hold content, and once,
// as an error in a loop would repeat it for every frame.
let crashed = false;
const crash = () => {
  if (!crashed) {
    crashed = true;
    log.report("web.crashed");
  }
};
window.addEventListener("error", crash);
window.addEventListener("unhandledrejection", crash);
log.info("app started");
const chatRef = chatRefOf(window.location.search);
// How long a hidden tab waits for a frame before its batch of run events goes out anyway.
const HIDDEN_MS = 250;
const TRANSCRIPT: Asks = { needs: "run.stream", offers: ["diff.view", "chat.events"] };
// No message carries members' names, so labels leave them out.
const NO_MEMBERS = new Map<string, string>();

function Root({ url }: { url: string }) {
  const [status, setStatus] = useState<Status>({ state: "connecting" });
  const [sync, setSync] = useState<Sync>(START);
  const [answers, setAnswers] = useState<Answers>(new Map());
  // The answers as last shown, for a late receipt to find which prompt it settles.
  const sent = useRef(answers);
  sent.current = answers;
  const [stopping, setStopping] = useState(false);
  const [late, setLate] = useState<Late>();
  const [changes, setChanges] = useState(NO_CHANGES);
  // The connection while it is live, and the commands sent on it that wait for an answer.
  const live = useRef<{ connection: Connection; receipts: Receipts; feed: ChatFeed | undefined }>(undefined);
  // The hello takes id 1.
  const lastId = useRef(1);
  useEffect(() => {
    // A closed socket's last news must not overwrite the next socket's.
    let current = true;
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
      if (!current) {
        return;
      }
      setLate({ request, settled });
      const answer = answeredUnder(sent.current, request);
      if (answer !== undefined && settled === "accepted") {
        live.current?.feed?.answered(answer.run, answer.prompt);
      }
      setAnswers((before) => settleLate(before, request, settled));
    });
    const schedule = nextFrame(
      () => document.hidden,
      (run) => requestAnimationFrame(run),
      (frame) => cancelAnimationFrame(frame),
      (run) => later(run, HIDDEN_MS),
    );
    const connection = new Connection(socket, ME, {
      status(next) {
        if (!current) {
          return;
        }
        setStatus(next);
        if (next.state === "live") {
          const feed =
            chatRef === undefined || !offers(next, "chat.events")
              ? undefined
              : new ChatFeed(chatRef, {
                  id: () => ++lastId.current,
                  send: (envelope) => connection.send(envelope),
                  schedule,
                  show: (shown) => current && setSync(shown),
                });
          live.current = { connection, receipts, feed };
          feed?.open();
          log.attach((envelope) => connection.send(envelope), offers(next, "log.forward"));
        } else if (next.state === "ended") {
          live.current?.feed?.end();
          live.current = undefined;
          setChanges(cut);
          log.detach();
          receipts.end();
          log.warn(next.code === "connection.lost" ? "connection lost" : "connection refused", { code: next.code });
        }
      },
      message(envelope) {
        route(envelope, {
          run: (message) => live.current?.feed?.push(message),
          chat: (message) => live.current?.feed?.push(message),
          answers: (message) => receipts.take(message),
          changes: (message) => {
            if (current) {
              setChanges((before) => receive(before, message));
            }
          },
        });
      },
    }, TRANSCRIPT, later);
    return () => {
      current = false;
      live.current = undefined;
      socket.close();
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
      setAnswers(next);
      void command({ kind: "permission", answer: { run, prompt, allow } }, request).then((sent) => {
        // An answer is far too short to be refused for its size.
        const settled = sent === "too_long" ? "refused" : sent;
        setAnswers((before) => settle(before, key, request, settled));
        if (settled === "accepted") {
          live.current?.feed?.answered(run, prompt);
        }
      });
    },
  };
  const stop = () => {
    setStopping(true);
    void command({ kind: "interrupt" }, newRequestId()).then(() => setStopping(false));
  };
  const { chat } = sync;
  const shown = {
    lines: chat === undefined ? [] : lines(chat, NO_MEMBERS),
    older: chat !== undefined && sync.live && chat.first > 1 ? () => live.current?.feed?.more() : undefined,
    answering,
    // A chat is on its way while the connection is made, then only from a server that serves chats.
    loading: chatRef !== undefined && chat === undefined && !sync.failed && (status.state === "connecting" || offers(status, "chat.events")),
    retry: sync.failed ? () => live.current?.feed?.open() : undefined,
    stop: chat !== undefined && working(chat) ? { stopping, stop } : undefined,
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
    <App me={ME} status={status} chat={shown} chatRef={chatRef} send={send} late={late} changes={changes} openChanges={openChanges} />
  );
}

const fake = fakeServerUrl(window.location);
const root = document.getElementById("root");
if (root !== null) {
  createRoot(root).render(
    <StrictMode>
      {fake === undefined ? <Shell fetch={fetch.bind(window)} /> : <Root url={fake} />}
    </StrictMode>,
  );
}
