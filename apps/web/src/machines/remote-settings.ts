import type { Later } from "../connection.ts";
import type { AgentKind, Envelope, MachineId, MachineSettings, MachineSettingsAction, MachineSettingsAnswerKind, RepoId, RequestId } from "../protocol.ts";
import { type Answer, MALFORMED } from "../api/api.ts";
import { type MachineSocket, askMachine } from "./ask-machine.ts";

/** `machine`'s settings for the socket's workspace, as they are now. */
export function current(socket: MachineSocket, machine: MachineId, later: Later): Promise<Answer<MachineSettings>> {
  return ask(socket, later, query(socket, machine, "current"), settingsIn);
}

/** The repositories `machine` has for the socket's workspace, sorted. */
export function repos(socket: MachineSocket, machine: MachineId, later: Later): Promise<Answer<RepoId[]>> {
  return ask(socket, later, query(socket, machine, "repos"), (answer) => (answer.kind === "repos" ? answer.repos : undefined));
}

/** The agents signed in on `machine` for the socket's workspace, which a member of a workspace it is shared with may ask too. */
export function agents(socket: MachineSocket, machine: MachineId, later: Later): Promise<Answer<AgentKind[]>> {
  return ask(socket, later, query(socket, machine, "agents"), (answer) => (answer.kind === "agents" ? answer.agents : undefined));
}

/** Changes `machine`'s settings for the socket's workspace; the answer is them after the change. */
export function change(
  socket: MachineSocket,
  machine: MachineId,
  action: MachineSettingsAction,
  later: Later,
): Promise<Answer<MachineSettings>> {
  const command = (id: number, request: RequestId): Envelope => ({
    id,
    type: "machine_settings_command",
    request,
    machine,
    workspace: socket.workspace,
    action,
  });
  return ask(socket, later, command, settingsIn);
}

function settingsIn(answer: MachineSettingsAnswerKind): MachineSettings | undefined {
  if (answer.kind !== "current") {
    return undefined;
  }
  const { kind, ...settings } = answer;
  return settings;
}

function query(socket: MachineSocket, machine: MachineId, kind: "current" | "repos" | "agents"): (id: number, request: RequestId) => Envelope {
  return (id, request) => ({ id, type: "machine_settings_query", request, machine, workspace: socket.workspace, query: { kind } });
}

// Asks with `envelope`; `read` takes the answer's body, or nothing when it is another kind.
function ask<T>(socket: MachineSocket, later: Later, envelope: (id: number, request: RequestId) => Envelope, read: (answer: MachineSettingsAnswerKind) => T | undefined): Promise<Answer<T>> {
  return askMachine(socket, later, envelope, (message, request) => {
    if (message.type !== "machine_settings_answer" || message.request !== request) {
      return undefined;
    }
    const value = read(message.answer);
    return value === undefined ? { ok: false, code: MALFORMED } : { ok: true, value };
  });
}
