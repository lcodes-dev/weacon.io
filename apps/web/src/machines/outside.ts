import type { Later } from "../connection.ts";
import type { ChatEntry, MachineId, OutsideSession, SessionsAnswerKind, SessionsQueryKind } from "../protocol.ts";
import { type Answer, MALFORMED } from "../api/api.ts";
import { type Chat, fromSnapshot } from "../chat/chat.ts";
import { type MachineSocket, askMachine } from "./ask-machine.ts";

export const MISSING = "session.missing";

/** An outside session read so far, and the core's number for its last event, which can skip where the chat's don't. */
export type Read = { chat: Chat; after: number };

/** The sessions the person ran outside Weacon on `machine`, newest first. */
export function sessions(socket: MachineSocket, machine: MachineId, later: Later): Promise<Answer<OutsideSession[]>> {
  return ask(socket, machine, { kind: "list" }, later, (answer) => (answer.kind === "list" ? { ok: true, value: answer.sessions } : undefined));
}

/**
 * `read`, or the start of `session` when nothing is, with every event after it that the core has
 * now; it asks for no more pages once `wanted` says no one waits for them.
 */
export async function readOn(
  socket: MachineSocket,
  machine: MachineId,
  session: OutsideSession,
  read: Read | undefined,
  later: Later,
  wanted: () => boolean,
): Promise<Answer<Read>> {
  const entries: ChatEntry[] = [...(read?.chat.entries ?? [])];
  let after = read?.after ?? 0;
  // Built once from every entry, since committing them one by one copies the chat each time.
  const done = () => ({ chat: fromSnapshot({ seq: entries.length, runs: new Map(), waiting: [], events: entries }), after });
  for (;;) {
    const query: SessionsQueryKind = { kind: "open", agent: session.agent, session: session.session, after };
    const page = await ask(socket, machine, query, later, (answer) => (answer.kind === "opened" ? { ok: true, value: answer } : undefined));
    if (!page.ok) {
      return page;
    }
    const fresh = page.value.events.filter((entry) => entry.seq > after);
    for (const { seq, event } of fresh) {
      entries.push({ seq: entries.length + 1, event });
      after = seq;
    }
    if (!page.value.more || fresh.length === 0 || !wanted()) {
      return { ok: true, value: done() };
    }
  }
}

function ask<T>(
  socket: MachineSocket,
  machine: MachineId,
  query: SessionsQueryKind,
  later: Later,
  take: (answer: SessionsAnswerKind) => Answer<T> | undefined,
): Promise<Answer<T>> {
  return askMachine(
    socket,
    later,
    (id, request) => ({ id, type: "sessions_query", request, machine, query }),
    (message, request) => {
      if (message.type !== "sessions_answer" || message.request !== request) {
        return undefined;
      }
      const { answer } = message;
      return answer.kind === "missing" ? { ok: false, code: MISSING } : (take(answer) ?? { ok: false, code: MALFORMED });
    },
  );
}
