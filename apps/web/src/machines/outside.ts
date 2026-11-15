import type { Later } from "../connection.ts";
import type { MachineId, OutsideSession, SessionsAnswerKind, SessionsQueryKind } from "../protocol.ts";
import { type Answer, MALFORMED } from "../api/api.ts";
import { type Chat, commit, fromSnapshot } from "../chat/chat.ts";
import { type MachineSocket, askMachine } from "./ask-machine.ts";

export const MISSING = "session.missing";

/**
 * An outside session as read so far: what it shows, and the core's number for the last event
 * read. The core leaves out an event too large to send, so its numbers can skip where the
 * chat's don't.
 */
export type Read = { chat: Chat; after: number };

/** The sessions the person ran outside Weacon on `machine`, newest first. */
export function sessions(socket: MachineSocket, machine: MachineId, later: Later): Promise<Answer<OutsideSession[]>> {
  return ask(socket, machine, { kind: "list" }, later, (answer) => (answer.kind === "list" ? { ok: true, value: answer.sessions } : undefined));
}

/** `read`, or the start of `session` when nothing is, with every event after it that the core has now. */
export async function readOn(
  socket: MachineSocket,
  machine: MachineId,
  session: OutsideSession,
  read: Read | undefined,
  later: Later,
): Promise<Answer<Read>> {
  let { chat, after } = read ?? { chat: fromSnapshot({ seq: 0, runs: new Map(), waiting: [], events: [] }), after: 0 };
  for (;;) {
    const query: SessionsQueryKind = { kind: "open", agent: session.agent, session: session.session, after };
    const page = await ask(socket, machine, query, later, (answer) => (answer.kind === "opened" ? { ok: true, value: answer } : undefined));
    if (!page.ok) {
      return page;
    }
    for (const { seq, event } of page.value.events.filter((entry) => entry.seq > after)) {
      chat = commit(chat, { seq: chat.seq + 1, event }) ?? chat;
      after = seq;
    }
    if (!page.value.more || page.value.events.length === 0) {
      return { ok: true, value: { chat, after } };
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
