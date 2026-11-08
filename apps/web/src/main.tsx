import { StrictMode, useEffect, useState } from "react";
import { createRoot } from "react-dom/client";

import "./app.css";
import { App } from "./app.tsx";
import { ME } from "./build.ts";
import { Connection, type Status } from "./connection.ts";
import { Feed } from "./feed.ts";
import { Log } from "./log.ts";
import { serverUrl } from "./server.ts";
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

function Root() {
  const [status, setStatus] = useState<Status>({ state: "connecting" });
  const [transcript, setTranscript] = useState(EMPTY);
  useEffect(() => {
    // A closed socket's last news must not overwrite the next socket's.
    let current = true;
    let socket: WebSocket;
    try {
      socket = new WebSocket(serverUrl(window.location));
    } catch {
      // Only a malformed `?server=` gets here.
      setStatus({ state: "ended", code: "connection.refused" });
      return;
    }
    const feed = new Feed(
      // A hidden tab gets no frames, so events would pile up until it's shown again.
      (flush) => (document.hidden ? setTimeout(flush, 250) : requestAnimationFrame(flush)),
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
        } else if (next.state === "ended") {
          log.detach();
          log.warn(next.code === "connection.lost" ? "connection lost" : "connection refused", { code: next.code });
        }
      },
      message: (envelope) => feed.push(envelope),
    });
    return () => {
      current = false;
      socket.close();
    };
  }, []);
  return <App me={ME} status={status} transcript={transcript} />;
}

const root = document.getElementById("root");
if (root !== null) {
  createRoot(root).render(
    <StrictMode>
      <Root />
    </StrictMode>,
  );
}
