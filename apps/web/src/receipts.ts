import type { Later } from "./connection.ts";
import type { Incoming } from "./incoming.ts";
import type { RequestId } from "./protocol.ts";

/**
 * How a command ended for its sender; `lost` and `unsure` may or may not have run. `offline` and
 * `detached` are refusals because the sender's machine is away or no longer has the chat, `full`
 * because the workspace has no room, `unsupported` because the machine's core can't do it, and
 * `working` because another agent is on a turn in the chat.
 */
export type Settled = "accepted" | "refused" | "offline" | "detached" | "full" | "unsupported" | "working" | "lost" | "unsure";

/**
 * What a command was about, for saying how it ended: its subject, what didn't happen when it was
 * refused or never left, what may not have, and what no longer happens once the sender's machine has lost the chat.
 */
export type Subject = { subject: string; refused: string; unsent: string; unsure: string; gone: string };

const capital = (words: string) => `${words.charAt(0).toUpperCase()}${words.slice(1)}`;

const SETTLED_TEXT: Record<Exclude<Settled, "accepted">, (about: Subject) => string> = {
  refused: ({ subject, refused }) => `${capital(subject)} ${refused}.`,
  offline: ({ subject, unsent }) => `The machine is offline, so ${subject} ${unsent}. Try again once it's back.`,
  detached: ({ gone }) => `Your agent's machine no longer has this chat, so ${gone}.`,
  full: ({ subject, unsent }) => `There's no room left, so ${subject} ${unsent}.`,
  unsupported: ({ subject, refused }) => `The machine's core can't do this, so ${subject} ${refused}.`,
  working: ({ subject, unsent }) => `Another person's agent is working in this chat, so ${subject} ${unsent}. Try again once it's done.`,
  lost: ({ subject, unsure }) => `The connection closed. ${capital(subject)} ${unsure}.`,
  unsure: ({ subject, unsure }) => `No answer came. ${capital(subject)} ${unsure}.`,
};

/** What a person reads once a command about `about` ended as `settled`: nothing when it was taken. */
export function settledText(settled: Settled, about: Subject): string | undefined {
  return settled === "accepted" ? undefined : SETTLED_TEXT[settled](about);
}

// How long a command waits for its machine's word on it, longer than the server waits.
const WAIT_MS = 40_000;

// A state a newer server adds is `unsure`.
const SETTLED = new Map<string, Settled>([
  ["accepted", "accepted"],
  ["completed", "accepted"],
  ["interrupted", "refused"],
  ["failed", "refused"],
  ["unknown", "unsure"],
]);

// The refusals a sender words on their own; any other code is `refused`.
const REFUSALS = new Map<string, Settled>([
  ["machine.offline", "offline"],
  ["chat.detached", "detached"],
  ["chat.full", "full"],
  ["message.unsupported", "unsupported"],
  ["chat.agent_working", "working"],
  ["machine.no_answer", "unsure"],
]);

type Waiting = { id: number; resolve: (settled: Settled) => void };

/**
 * Holds each sent command until its first receipt, an error in answer to its envelope, the end
 * of the connection, or a deadline.
 */
export class Receipts {
  #waiting = new Map<RequestId, Waiting>();
  #later: Later;

  constructor(later: Later) {
    this.#later = later;
  }

  /** How the command sent under `request`, in the envelope `id`, ends. */
  wait(request: RequestId, id: number): Promise<Settled> {
    const settled = new Promise<Settled>((resolve) => {
      this.#waiting.set(request, { id, resolve });
    });
    this.#later(() => this.settle(request, "unsure"), WAIT_MS);
    return settled;
  }

  /** Settles the command `envelope` answers, if any. */
  take(envelope: Incoming): void {
    if (envelope.type === "receipt") {
      const failed = envelope.state === "failed" ? REFUSALS.get(envelope.code) : undefined;
      this.settle(envelope.request, failed ?? SETTLED.get(envelope.state) ?? "unsure");
      return;
    }
    if (envelope.type !== "error") {
      return;
    }
    const refused = [...this.#waiting].find(([, waiting]) => waiting.id === envelope.id);
    if (refused !== undefined) {
      this.settle(refused[0], REFUSALS.get(envelope.code) ?? "refused");
    }
  }

  /** Settles every command still waiting as lost, as when the connection ends. */
  end(): void {
    for (const waiting of this.#waiting.values()) {
      waiting.resolve("lost");
    }
    this.#waiting.clear();
  }

  /** Ends the wait for `request` as `settled`, as when its command couldn't be sent. */
  settle(request: RequestId, settled: Settled): void {
    this.#waiting.get(request)?.resolve(settled);
    this.#waiting.delete(request);
  }
}
