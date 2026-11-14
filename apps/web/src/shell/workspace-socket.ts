import { createContext, useContext, useSyncExternalStore } from "react";

import { HELLO_ID } from "../connection.ts";
import type { Incoming } from "../incoming.ts";
import type { Envelope, WorkspaceId } from "../protocol.ts";

/** The open workspace's one socket, which its pages send on and listen to, across the feed's reconnects. */
export class WorkspaceSocket {
  readonly workspace: WorkspaceId;
  #send: ((envelope: Envelope) => void) | undefined;
  #handlers = new Set<(message: Incoming) => void>();
  #lastId = HELLO_ID;
  #offered: readonly string[] = [];
  #watchers = new Set<() => void>();

  constructor(workspace: WorkspaceId) {
    this.workspace = workspace;
  }

  /**
   * Sends through `send` from now on, as when the feed goes live with what the server `offered`;
   * `undefined` when it ends, keeping what was offered until the next welcome says otherwise.
   */
  attach(send: ((envelope: Envelope) => void) | undefined, offered?: readonly string[]): void {
    this.#send = send;
    if (offered !== undefined && offered !== this.#offered) {
      this.#offered = offered;
      for (const watcher of [...this.#watchers]) {
        watcher();
      }
    }
  }

  /** Calls `watcher` each time what the server offers changes, until the returned function is called. */
  watch(watcher: () => void): () => void {
    this.#watchers.add(watcher);
    return () => this.#watchers.delete(watcher);
  }

  /** Whether the server offered `capability` when it last welcomed the feed. */
  offers(capability: string): boolean {
    return this.#offered.includes(capability);
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

/** Whether the server offers `capability` on `socket`, kept current as welcomes change it. */
export function useOffers(socket: WorkspaceSocket | undefined, capability: string): boolean {
  return useSyncExternalStore(
    (changed) => socket?.watch(changed) ?? (() => {}),
    () => socket?.offers(capability) ?? false,
  );
}
