import type { Later } from "../connection.ts";
import { newId } from "../ids.ts";
import type { Incoming } from "../incoming.ts";
import type { Envelope, RequestId } from "../protocol.ts";
import { type Answer, UNREACHABLE } from "../api/api.ts";
import type { WorkspaceSocket } from "../shell/workspace-socket.ts";

/** The open workspace's socket, through which the server reaches the person's machines. */
export type MachineSocket = Pick<WorkspaceSocket, "workspace" | "send" | "subscribe" | "nextId">;

// The server answers `machine.no_answer` after 30 s; this is only for a server that never does.
const ANSWER_MS = 35_000;
export const NO_ANSWER = "machine.no_answer";
export const RECONNECTING = "workspace.reconnecting";

/**
 * Asks a machine through `socket` with the envelope `ask` makes. The core's answer echoes the
 * request, and `take` reads it; an error answers the envelope by its id.
 */
export function askMachine<T>(
  socket: MachineSocket,
  later: Later,
  ask: (id: number, request: RequestId) => Envelope,
  take: (message: Incoming, request: RequestId) => Answer<T> | undefined,
): Promise<Answer<T>> {
  const id = socket.nextId();
  const request = newId();
  return new Promise((resolve) => {
    let finished = false;
    let unsubscribe = () => {};
    const finish = (answer: Answer<T>) => {
      if (!finished) {
        finished = true;
        unsubscribe();
        resolve(answer);
      }
    };
    unsubscribe = socket.subscribe((message) => {
      const answer = message.type === "error" && message.id === id ? { ok: false as const, code: message.code } : take(message, request);
      if (answer !== undefined) {
        finish(answer);
      }
    });
    later(() => finish({ ok: false, code: NO_ANSWER }), ANSWER_MS);
    try {
      if (!socket.send(ask(id, request))) {
        finish({ ok: false, code: RECONNECTING });
      }
    } catch {
      finish({ ok: false, code: UNREACHABLE });
    }
  });
}
