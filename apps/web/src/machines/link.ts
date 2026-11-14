import { newRequestId } from "../ids.ts";
import type {
  Envelope,
  MachineId,
  MachineSettings,
  MachineSettingsAction,
  WorkspaceId,
} from "../protocol.ts";
import type { Answer } from "../shell/api.ts";
import { UNREACHABLE } from "../shell/api.ts";

/** A connection that reaches the person's machines through the server. */
export type Link = {
  /** Puts `envelope` on the connection; its `id` is new on this connection. */
  send(envelope: Envelope): void;
  /** Hands every envelope that arrives to `handle`, until the returned function is called. */
  subscribe(handle: (envelope: Envelope) => void): () => void;
  /** An envelope id not yet used on this connection. */
  nextId(): number;
};

/** Starts `run` after `ms` and returns what stops it; tests pass their own. */
export type Wait = (run: () => void, ms: number) => () => void;

// A core that is offline never answers; the page says so rather than wait for good.
const ANSWER_MS = 30_000;
export const NO_ANSWER = "settings.no_answer";

/** Where the settings are: one machine, as it is set up for one workspace. */
export type Place = { machine: MachineId; workspace: WorkspaceId };

/** The settings as they are now. */
export function current(link: Link, place: Place, wait: Wait): Promise<Answer<MachineSettings>> {
  const request = newRequestId();
  return ask(link, wait, request, {
    id: link.nextId(),
    type: "machine_settings_query",
    request,
    ...place,
    query: { kind: "current" },
  });
}

/** Changes the settings; the answer is them after the change. */
export function change(
  link: Link,
  place: Place,
  action: MachineSettingsAction,
  wait: Wait,
): Promise<Answer<MachineSettings>> {
  const request = newRequestId();
  return ask(link, wait, request, { id: link.nextId(), type: "machine_settings_command", request, ...place, action });
}

// The core's answer echoes `request`; an error answers the envelope by its id.
function ask(link: Link, wait: Wait, request: string, sent: Envelope): Promise<Answer<MachineSettings>> {
  return new Promise((resolve) => {
    let finished = false;
    let unsubscribe = () => {};
    let cancel = () => {};
    const finish = (answer: Answer<MachineSettings>) => {
      if (!finished) {
        finished = true;
        unsubscribe();
        cancel();
        resolve(answer);
      }
    };
    unsubscribe = link.subscribe((envelope) => {
      if (envelope.type === "machine_settings_answer" && envelope.request === request) {
        const { kind, ...settings } = envelope.answer;
        finish(kind === "current" ? { ok: true, value: settings } : { ok: false, code: "answer.malformed" });
      } else if (envelope.type === "error" && envelope.id === sent.id) {
        finish({ ok: false, code: envelope.code });
      }
    });
    cancel = wait(() => finish({ ok: false, code: NO_ANSWER }), ANSWER_MS);
    try {
      link.send(sent);
    } catch {
      finish({ ok: false, code: UNREACHABLE });
    }
  });
}
