import { StrictMode, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";

import "./app.css";
import { App } from "./app.tsx";
import { ME } from "./build.ts";
import { type Send, deliver, prompt } from "./composer/prompt.ts";
import { Connection, type Status } from "./connection.ts";
import { Feed, nextFrame } from "./feed.ts";
import { Log } from "./log.ts";
import { placeOf } from "./place.ts";
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
const place = placeOf(window.location.search);

function Root({ url }: { url: string }) {
  const [status, setStatus] = useState<Status>({ state: "connecting" });
  const [transcript, setTranscript] = useState(EMPTY);
  const [send, setSend] = useState<Send>();
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
    const receipts = new Receipts((run, ms) => setTimeout(run, ms));
    const feed = new Feed(
      nextFrame(
        () => document.hidden,
        (run) => requestAnimationFrame(run),
        (frame) => cancelAnimationFrame(frame),
        (run) => setTimeout(run, 250),
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
          log.attach((envelope) => connection.send(envelope), next.capabilities.includes("log.forward"));
          if (place !== undefined) {
            // The hello took id 1.
            let lastId = 1;
            const sendTo: Send = (text, request) =>
              deliver(prompt(++lastId, place, request, text), receipts, (envelope) => connection.send(envelope));
            setSend(() => sendTo);
          }
        } else if (next.state === "ended") {
          log.detach();
          receipts.end();
          log.warn(next.code === "connection.lost" ? "connection lost" : "connection refused", { code: next.code });
        }
      },
      message(envelope) {
        if (!receipts.take(envelope)) {
          feed.push(envelope);
        }
      },
    });
    return () => {
      current = false;
      socket.close();
    };
  }, [url]);
  return <App me={ME} status={status} transcript={transcript} place={place} send={send} />;
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
