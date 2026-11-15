import type { Later } from "../connection.ts";
import type { Envelope, MachineId, MachineSettings, MachineSettingsAction, RequestId } from "../protocol.ts";
import { type Answer, MALFORMED } from "../api/api.ts";
import { type MachineSocket, askMachine } from "./ask-machine.ts";

/** `machine`'s settings for the socket's workspace, as they are now. */
export function current(socket: MachineSocket, machine: MachineId, later: Later): Promise<Answer<MachineSettings>> {
  return ask(socket, later, (id, request) => ({
    id,
    type: "machine_settings_query",
    request,
    machine,
    workspace: socket.workspace,
    query: { kind: "current" },
  }));
}

/** Changes `machine`'s settings for the socket's workspace; the answer is them after the change. */
export function change(
  socket: MachineSocket,
  machine: MachineId,
  action: MachineSettingsAction,
  later: Later,
): Promise<Answer<MachineSettings>> {
  return ask(socket, later, (id, request) => ({
    id,
    type: "machine_settings_command",
    request,
    machine,
    workspace: socket.workspace,
    action,
  }));
}

function ask(socket: MachineSocket, later: Later, envelope: (id: number, request: RequestId) => Envelope): Promise<Answer<MachineSettings>> {
  return askMachine(socket, later, envelope, (message, request) => {
    if (message.type !== "machine_settings_answer" || message.request !== request) {
      return undefined;
    }
    const { answer } = message;
    if (answer.kind !== "current") {
      return { ok: false, code: MALFORMED };
    }
    const { kind, ...settings } = answer;
    return { ok: true, value: settings };
  });
}
