import type { Later } from "./connection.ts";
import type { Incoming } from "./incoming.ts";
import type { RequestId } from "./protocol.ts";

/** How a command ended for its sender; `lost` and `unsure` may or may not have run. */
export type Settled = "accepted" | "refused" | "lost" | "unsure";

// How long a command waits for the server's word on it.
const WAIT_MS = 15_000;

// A state a newer server adds is `unsure`.
const SETTLED = new Map<string, Settled>([
  ["accepted", "accepted"],
  ["completed", "accepted"],
  ["interrupted", "refused"],
  ["failed", "refused"],
  ["unknown", "unsure"],
]);

type Waiting = { id: number; settled: Promise<Settled>; resolve: (settled: Settled) => void };

/**
 * Holds each sent command until its first receipt, an error in answer to its envelope, the end
 * of the connection, or a deadline.
 */
export class Receipts {
  #waiting = new Map<RequestId, Waiting>();
  // Commands whose wait ran out before their receipt came, with the envelope each went in.
  #overdue = new Map<RequestId, number>();
  #later: Later;
  #late: (request: RequestId, settled: Settled) => void;

  /** `late` hears how a command ended when its receipt comes after its wait ran out. */
  constructor(later: Later, late: (request: RequestId, settled: Settled) => void = () => {}) {
    this.#later = later;
    this.#late = late;
  }

  /** How the command sent under `request`, in the envelope `id`, ends. */
  wait(request: RequestId, id: number): Promise<Settled> {
    const already = this.#waiting.get(request);
    if (already !== undefined) {
      return already.settled;
    }
    let resolve: (settled: Settled) => void = () => {};
    const settled = new Promise<Settled>((done) => {
      resolve = done;
    });
    const waiting = { id, settled, resolve };
    this.#waiting.set(request, waiting);
    this.#later(() => {
      // Only this wait's deadline, not that of an earlier send of the same request.
      if (this.#waiting.get(request) === waiting) {
        this.settle(request, "unsure");
        this.#overdue.set(request, id);
      }
    }, WAIT_MS);
    return settled;
  }

  /** Settles the command `envelope` answers, if any. */
  take(envelope: Incoming): void {
    if (envelope.type === "receipt") {
      const settled = SETTLED.get(envelope.state) ?? "unsure";
      if (this.#overdue.delete(envelope.request) && !this.#waiting.has(envelope.request)) {
        this.#late(envelope.request, settled);
      }
      this.settle(envelope.request, settled);
      return;
    }
    if (envelope.type !== "error") {
      return;
    }
    const refused = [...this.#waiting].find(([, waiting]) => waiting.id === envelope.id);
    if (refused !== undefined) {
      this.settle(refused[0], "refused");
      return;
    }
    const overdue = [...this.#overdue].find(([, id]) => id === envelope.id);
    // An error about an earlier envelope doesn't decide a retry of the same request that waits.
    if (overdue !== undefined) {
      this.#overdue.delete(overdue[0]);
      if (!this.#waiting.has(overdue[0])) {
        this.#late(overdue[0], "refused");
      }
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
