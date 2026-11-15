import type { TerminalCommand, TerminalUpdate } from "../protocol.ts";
import type { WorkspaceSocket } from "../shell/workspace-socket.ts";

export type LinkSocket = Pick<WorkspaceSocket, "send" | "subscribe" | "nextId" | "watch" | "connection" | "live">;

/** A sign-in terminal's way to its machine: the open workspace's socket, while it stays connected. */
export function terminalLink(socket: LinkSocket) {
  return {
    send: (command: TerminalCommand) => socket.send({ id: socket.nextId(), type: "terminal_command", ...command }),
    subscribe: (handle: (update: TerminalUpdate) => void) =>
      socket.subscribe((message) => {
        if (message.type === "terminal") {
          handle(message);
        }
      }),
    /** Calls `handle` once the connection the terminal opened on has ended, as the server forgets its terminals then. */
    lost: (handle: () => void) => {
      const opened = socket.connection;
      return socket.watch(() => {
        if (!socket.live || socket.connection !== opened) {
          handle();
        }
      });
    },
  };
}
