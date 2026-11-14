import { useEffect, useRef, useState } from "react";

import { Connection, type Status } from "../connection.ts";
import type { Incoming } from "../incoming.ts";
import { ME } from "../me.ts";
import type { WorkspaceId } from "../protocol.ts";
import type { Api } from "./api.ts";
import { FEED, STAYED_MS, failedAfter, feedUrl, heardAfter, retries, retryAfter } from "./workspace-feed.ts";
import type { WorkspaceSocket } from "./workspace-socket.ts";

/**
 * Where `workspace`'s feed stands, and what its server offered when it last welcomed it; it hands
 * each message to `socket`, a workspace change only once, and opens again after the last one heard.
 */
export function useFeed(workspace: WorkspaceId, api: Api, socket: WorkspaceSocket): { status: Status; offered: readonly string[] | undefined } {
  const [status, setStatus] = useState<Status>({ state: "connecting" });
  // Kept apart from `status`, which a feed that ends at once may replace before it is shown.
  const [offered, setOffered] = useState<readonly string[]>();
  useEffect(() => {
    let current = true;
    let since = 0;
    let failed = 0;
    // Whether the open feed has stayed long enough, or brought a change, to restart the backoff.
    let stayed = false;
    let staying: ReturnType<typeof setTimeout> | undefined;
    let opened: WebSocket | undefined;
    let retry: ReturnType<typeof setTimeout> | undefined;
    const open = () => {
      if (failed > 0) {
        setStatus({ state: "connecting" });
      }
      const fresh = new WebSocket(feedUrl(window.location, workspace, since));
      opened = fresh;
      const listener = {
        status(next: Status) {
          if (!current) {
            return;
          }
          setStatus(next);
          if (next.state === "live") {
            setOffered(next.capabilities);
            stayed = false;
            staying = setTimeout(() => (stayed = true), STAYED_MS);
            socket.attach((envelope) => connection.send(envelope));
          } else if (next.state === "ended") {
            clearTimeout(staying);
            socket.attach(undefined);
            // Asking for the session lets one that ended reach sign-in.
            void api.session();
            failed = failedAfter(failed, stayed);
            stayed = false;
            if (retries(next.code)) {
              retry = setTimeout(open, retryAfter(failed));
            }
          }
        },
        message(envelope: Incoming) {
          const after = current ? heardAfter(envelope, workspace, since) : undefined;
          if (after === undefined) {
            return;
          }
          stayed ||= after > since;
          since = after;
          socket.heard(envelope);
        },
      };
      const connection = new Connection(fresh, ME, listener, FEED, (run, ms) => setTimeout(run, ms));
    };
    open();
    return () => {
      current = false;
      clearTimeout(retry);
      clearTimeout(staying);
      socket.attach(undefined);
      opened?.close();
    };
  }, [workspace, api, socket]);
  return { status, offered };
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
