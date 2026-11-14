import { StrictMode, useEffect, useRef, useState } from "react";
import { createRoot } from "react-dom/client";

import "./app.css";
import { App } from "./app.tsx";
import { chatRefOf } from "./chat-ref.ts";
import { type Send, deliver, prompt } from "./composer/prompt.ts";
import { Connection, type Later, type Status, offers } from "./connection.ts";
import { NO_CHANGES, ask, cut, needsAsking, receive } from "./diff/changes.ts";
import { Feed, nextFrame } from "./feed.ts";
import { newRequestId } from "./ids.ts";
import { Log } from "./log.ts";
import { ME } from "./me.ts";
import { Receipts } from "./receipts.ts";
import { fakeServerUrl } from "./server.ts";
import { Api } from "./shell/api.ts";
import { Shell } from "./shell/shell.tsx";
import { EMPTY, apply } from "./transcript.ts";

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

function Root({ url }: { url: string }) {
  const [status, setStatus] = useState<Status>({ state: "connecting" });
  const [transcript, setTranscript] = useState(EMPTY);
  const [changes, setChanges] = useState(NO_CHANGES);
  // The connection while it is live, and the commands sent on it that wait for an answer.
  const live = useRef<{ connection: Connection; receipts: Receipts }>(undefined);
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
    const receipts = new Receipts(later);
    const feed = new Feed(
      nextFrame(
        () => document.hidden,
        (run) => requestAnimationFrame(run),
        (frame) => cancelAnimationFrame(frame),
        (run) => later(run, HIDDEN_MS),
      ),
      (events) => {
        if (current) {
          setTranscript((before) => apply(before, events));
        }
      },
    );
    const connection = new Connection(socket, ME, {
      status(next) {
        if (!current) {
          return;
        }
        setStatus(next);
        if (next.state === "live") {
          live.current = { connection, receipts };
          log.attach((envelope) => connection.send(envelope), offers(next, "log.forward"));
        } else if (next.state === "ended") {
          live.current = undefined;
          setChanges(cut);
          log.detach();
          receipts.end();
          log.warn(next.code === "connection.lost" ? "connection lost" : "connection refused", { code: next.code });
        }
      },
      message(envelope) {
        switch (envelope.type) {
          case "run":
            feed.push(envelope);
            return;
          case "receipt":
            receipts.take(envelope);
            return;
          case "diff_answer":
            if (current) {
              setChanges((before) => receive(before, envelope));
            }
            return;
          case "error":
            // It may answer the diff query or a prompt; each keeps only what is for it.
            receipts.take(envelope);
            if (current) {
              setChanges((before) => receive(before, envelope));
            }
            return;
          default:
            return;
        }
      },
    }, later);
    return () => {
      current = false;
      live.current = undefined;
      socket.close();
    };
  }, [url]);
  const send: Send = (text, request) => {
    const now = live.current;
    if (now === undefined || chatRef === undefined) {
      return Promise.resolve("lost");
    }
    return deliver(prompt(++lastId.current, chatRef, request, text), now.receipts, (envelope) => now.connection.send(envelope));
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
    <App me={ME} status={status} transcript={transcript} chatRef={chatRef} send={send} changes={changes} openChanges={openChanges} />
  );
}

const fake = fakeServerUrl(window.location);
const root = document.getElementById("root");
if (root !== null) {
  createRoot(root).render(
    <StrictMode>
      {fake === undefined ? <Shell api={new Api(fetch.bind(window))} /> : <Root url={fake} />}
    </StrictMode>,
  );
}
