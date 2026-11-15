import type { RequestId } from "../protocol.ts";
import { mayHaveRun } from "../receipts.ts";
import { type Sent, sentProblem } from "./prompt.ts";

/** How one send ended: what it carried, whether it was taken, and, for the latest send, why it may not have been. */
export type Ended<T> = { payload: T; taken: boolean; latest: boolean; problem: string | undefined };

/**
 * The sends of one box, each with the `payload` it carries: the same text goes again under the same
 * request while its last send may have run, so it can't run twice. A send's answer and its late
 * receipt settle it alike, and only the first to come counts.
 */
export class Sends<T> {
  #held = new Map<RequestId, { text: string; payload: T }>();
  #latest: RequestId | undefined;

  /** The request to send `text` under: that of a send of the same text that may have run, or a fresh one. */
  start(text: string, payload: T, fresh: () => RequestId): RequestId {
    const request = [...this.#held].find(([, held]) => held.text === text)?.[0] ?? fresh();
    this.#held.set(request, { text, payload });
    this.#latest = request;
    return request;
  }

  /** How the send under `request` ended, as its answer or late receipt says; `undefined` once it is settled. */
  settle(request: RequestId, sent: Sent): Ended<T> | undefined {
    const held = this.#held.get(request);
    if (held === undefined) {
      return undefined;
    }
    if (sent === "too_long" || !mayHaveRun(sent)) {
      this.#held.delete(request);
    }
    return { payload: held.payload, taken: sent === "accepted", latest: request === this.#latest, problem: sentProblem(sent) };
  }
}
