import type { TerminalCommand, TerminalUpdate } from "../protocol.ts";
import type { WorkspaceSocket } from "../shell/workspace-socket.ts";

/** A sign-in terminal's way to its machine: the open workspace's socket. */
export function terminalLink(socket: Pick<WorkspaceSocket, "send" | "subscribe" | "nextId">) {
  return {
    send: (command: TerminalCommand) => socket.send({ id: socket.nextId(), type: "terminal_command", ...command }),
    subscribe: (handle: (update: TerminalUpdate) => void) =>
      socket.subscribe((message) => {
        if (message.type === "terminal") {
          handle(message);
        }
      }),
  };
}
