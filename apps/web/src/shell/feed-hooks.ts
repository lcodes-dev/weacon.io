import { useEffect, useRef, useState } from "react";

import type { Api } from "../api/api.ts";
import { Connection, type Status, later, offers } from "../connection.ts";
import type { Incoming } from "../incoming.ts";
import type { Log } from "../log.ts";
import { ME } from "../me.ts";
import type { WorkspaceChange, WorkspaceId } from "../protocol.ts";
import { FEED, STAYED_MS, failedAfter, feedUrl, heardAfter, retries, retryAfter } from "./workspace-feed.ts";
import type { WorkspaceSocket } from "./workspace-socket.ts";

/**
 * Where `workspace`'s feed stands; it gives `socket` what the server offered at each welcome, hands
 * each message to `socket`, a workspace change only once, and opens again after the last one heard.
 * While it is live, `log` forwards through it.
 */
export function useFeed(workspace: WorkspaceId, api: Api, socket: WorkspaceSocket, log: Log): Status {
  const [status, setStatus] = useState<Status>({ state: "connecting" });
  // Kept apart from `status`, which a feed that ends at once may replace before it is shown.
  useEffect(() => {
    let since = 0;
    let failed = 0;
    // Whether the open feed has stayed long enough, or brought a change, to restart the backoff.
    let stayed = false;
    let staying: ReturnType<typeof setTimeout> | undefined;
    let opened: Connection | undefined;
    let retry: ReturnType<typeof setTimeout> | undefined;
    const open = () => {
      if (failed > 0) {
        setStatus({ state: "connecting" });
      }
      const fresh = new WebSocket(feedUrl(window.location, workspace, since));
      const listener = {
        status(next: Status) {
          setStatus(next);
          if (next.state === "live") {
            stayed = false;
            staying = setTimeout(() => (stayed = true), STAYED_MS);
            socket.attach((envelope) => connection.send(envelope), next.capabilities);
            log.attach((envelope) => connection.send(envelope), offers(next, "log.forward"));
          } else if (next.state === "ended") {
            clearTimeout(staying);
            socket.attach(undefined);
            log.detach();
            // Asking for the workspace lets a session that ended reach sign-in, and a member removed leave it.
            void api.workspace(workspace);
            failed = failedAfter(failed, stayed);
            stayed = false;
            if (retries(next.code)) {
              retry = setTimeout(open, retryAfter(failed));
            }
          }
        },
        message(envelope: Incoming) {
          const after = heardAfter(envelope, workspace, since);
          if (after === undefined) {
            return;
          }
          stayed ||= after > since;
          since = after;
          socket.heard(envelope);
        },
      };
      const connection = new Connection(fresh, ME, listener, FEED, later);
      opened = connection;
    };
    open();
    return () => {
      clearTimeout(retry);
      clearTimeout(staying);
      socket.attach(undefined);
      log.detach();
      opened?.close();
    };
  }, [workspace, api, socket, log]);
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

/** Calls `load`, at most once per burst, each time `socket` hears a change of its workspace that `matters`. */
export function useReloadOn(socket: WorkspaceSocket, matters: (change: WorkspaceChange) => boolean, load: () => void, deps: unknown[] = []): void {
  const reload = useBurst(load);
  useEffect(
    () =>
      socket.subscribe((message) => {
        if (message.type === "workspace" && matters(message.change)) {
          reload();
        }
      }),
    [socket, ...deps],
  );
}
