import { useEffect, useRef, useState } from "react";

import { Connection, type Status } from "../connection.ts";
import type { Incoming } from "../incoming.ts";
import { ME } from "../me.ts";
import type { WorkspaceId } from "../protocol.ts";
import type { Api } from "./api.ts";
import { FEED, feedUrl, retries, retryAfter } from "./workspace-feed.ts";
import type { WorkspaceSocket } from "./workspace-socket.ts";

/**
 * Where `workspace`'s feed stands; it hands each message to `socket`, a workspace change only once,
 * and opens again after the last change it heard.
 */
export function useFeed(workspace: WorkspaceId, api: Api, socket: WorkspaceSocket): Status {
  const [status, setStatus] = useState<Status>({ state: "connecting" });
  useEffect(() => {
    let current = true;
    let since = 0;
    let failed = 0;
    let opened: WebSocket | undefined;
    let retry: ReturnType<typeof setTimeout> | undefined;
    const open = () => {
      const fresh = new WebSocket(feedUrl(window.location, workspace, since));
      opened = fresh;
      const listener = {
        status(next: Status) {
          if (!current) {
            return;
          }
          setStatus(next);
          if (next.state === "live") {
            failed = 0;
            socket.attach((envelope) => connection.send(envelope));
          } else if (next.state === "ended") {
            socket.attach(undefined);
            // Asking for the session lets one that ended reach sign-in.
            void api.session();
            if (retries(next.code)) {
              failed += 1;
              retry = setTimeout(open, retryAfter(failed));
            }
          }
        },
        message(envelope: Incoming) {
          if (!current || (envelope.type === "workspace" && (envelope.workspace !== workspace || envelope.seq <= since))) {
            return;
          }
          if (envelope.type === "workspace") {
            since = envelope.seq;
          }
          socket.heard(envelope);
        },
      };
      const connection = new Connection(fresh, ME, listener, FEED, (run, ms) => setTimeout(run, ms));
    };
    open();
    return () => {
      current = false;
      clearTimeout(retry);
      socket.attach(undefined);
      opened?.close();
    };
  }, [workspace, api, socket]);
  return status;
}

const BURST_MS = 250;

/** `run`, at most once per burst of calls, `BURST_MS` after the first. */
export function useBurst(run: () => void): () => void {
  const latest = useRef(run);
  latest.current = run;
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  return () => {
    if (timer.current === undefined) {
      timer.current = setTimeout(() => {
        timer.current = undefined;
        latest.current();
      }, BURST_MS);
    }
  };
}
