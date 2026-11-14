import { createContext, useContext } from "react";

import { HELLO_ID } from "../connection.ts";
import type { Incoming } from "../incoming.ts";
import type { Envelope } from "../protocol.ts";

/** The open workspace's one socket, which its pages send on and listen to, across the feed's reconnects. */
export class WorkspaceSocket {
  #send: ((envelope: Envelope) => void) | undefined;
  #handlers = new Set<(message: Incoming) => void>();
  #lastId = HELLO_ID;

  /** Sends through `send` from now on, as when the feed goes live; `undefined` when it ends. */
  attach(send: ((envelope: Envelope) => void) | undefined): void {
    this.#send = send;
  }

  /** Hands `message` to every page that subscribed. */
  heard(message: Incoming): void {
    for (const handle of [...this.#handlers]) {
      handle(message);
    }
  }

  /** Sends `envelope`; while the feed isn't live it is dropped, and this says so with `false`. */
  send(envelope: Envelope): boolean {
    this.#send?.(envelope);
    return this.#send !== undefined;
  }

  /** Calls `handle` with each checked message from the feed, until the returned function is called. */
  subscribe(handle: (message: Incoming) => void): () => void {
    this.#handlers.add(handle);
    return () => this.#handlers.delete(handle);
  }

  /** The next envelope id for a request. */
  nextId(): number {
    return ++this.#lastId;
  }
}

/** The open workspace's socket, for its pages. */
export const WorkspaceSocketContext = createContext<WorkspaceSocket | undefined>(undefined);

/** The open workspace's socket; only a page of the open workspace may ask for it. */
export function useWorkspaceSocket(): WorkspaceSocket {
  const socket = useContext(WorkspaceSocketContext);
  if (socket === undefined) {
    throw new Error("useWorkspaceSocket outside an open workspace");
  }
  return socket;
}
