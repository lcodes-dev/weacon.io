import type { Later } from "../connection.ts";
import { newRequestId } from "../ids.ts";
import type { Incoming } from "../incoming.ts";
import type { Envelope, MachineId, MachineSettings, MachineSettingsAction, RequestId, WorkspaceId } from "../protocol.ts";
import { type Answer, MALFORMED, UNREACHABLE } from "../shell/api.ts";

/** The open workspace's socket, through which the server reaches the person's machines. */
export type Link = {
  readonly workspace: WorkspaceId;
  /** Puts `envelope` on the socket; false, dropping it, while the socket isn't live. */
  send(envelope: Envelope): boolean;
  /** Hands every checked message that arrives to `handle`, until the returned function is called. */
  subscribe(handle: (message: Incoming) => void): () => void;
  /** An envelope id not yet used on this socket. */
  nextId(): number;
};

// The server answers `machine.no_answer` after 30 s; this is only for a server that never does.
const ANSWER_MS = 35_000;
export const NO_ANSWER = "machine.no_answer";
export const RECONNECTING = "workspace.reconnecting";

/** `machine`'s settings for the link's workspace, as they are now. */
export function current(link: Link, machine: MachineId, later: Later): Promise<Answer<MachineSettings>> {
  return ask(link, later, (id, request) => ({
    id,
    type: "machine_settings_query",
    request,
    machine,
    workspace: link.workspace,
    query: { kind: "current" },
  }));
}

/** Changes `machine`'s settings for the link's workspace; the answer is them after the change. */
export function change(
  link: Link,
  machine: MachineId,
  action: MachineSettingsAction,
  later: Later,
): Promise<Answer<MachineSettings>> {
  return ask(link, later, (id, request) => ({
    id,
    type: "machine_settings_command",
    request,
    machine,
    workspace: link.workspace,
    action,
  }));
}

// The core's answer echoes the request; an error answers the envelope by its id.
function ask(link: Link, later: Later, envelope: (id: number, request: RequestId) => Envelope): Promise<Answer<MachineSettings>> {
  const id = link.nextId();
  const request = newRequestId();
  return new Promise((resolve) => {
    let finished = false;
    let unsubscribe = () => {};
    const finish = (answer: Answer<MachineSettings>) => {
      if (!finished) {
        finished = true;
        unsubscribe();
        resolve(answer);
      }
    };
    unsubscribe = link.subscribe((message) => {
      if (message.type === "machine_settings_answer" && message.request === request) {
        const { kind, ...settings } = message.answer;
        finish(kind === "current" ? { ok: true, value: settings } : { ok: false, code: MALFORMED });
      } else if (message.type === "error" && message.id === id) {
        finish({ ok: false, code: message.code });
      }
    });
    later(() => finish({ ok: false, code: NO_ANSWER }), ANSWER_MS);
    try {
      if (!link.send(envelope(id, request))) {
        finish({ ok: false, code: RECONNECTING });
      }
    } catch {
      finish({ ok: false, code: UNREACHABLE });
    }
  });
}
