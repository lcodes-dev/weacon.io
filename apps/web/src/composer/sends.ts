import type { RequestId } from "../protocol.ts";
import { type Late, type Retry, type Sent, after, requestFor } from "./prompt.ts";

/** How one send ended: whether it was taken, and, for the latest send, why it may not have been. */
export type Ended = { text: string; taken: boolean; latest: boolean; problem: string | undefined };

/**
 * The sends of one box: the same text goes again under the same request so it can't run twice, and
 * each send that may have run is settled by its late receipt, even while a retry of it is out.
 */
export class Sends {
  #latest: Retry | undefined;
  #retry: Retry | undefined;
  // Sends that may have run, by request; the send on its way, and whether a late receipt accepted it.
  #unsure = new Map<RequestId, string>();
  #out: { request: RequestId; accepted: boolean } | undefined;

  /** The request to send `text` under. */
  start(text: string, fresh: () => RequestId): RequestId {
    const request = requestFor(this.#retry, text, fresh);
    this.#latest = { text, request };
    this.#out = { request, accepted: false };
    return request;
  }

  /** How the send of `text` under `request` ended, once its answer came as `sent`. */
  end(text: string, request: RequestId, sent: Sent): Ended {
    const accepted = this.#out?.request === request && this.#out.accepted;
    this.#out = undefined;
    return this.#settle(text, request, accepted ? "accepted" : sent);
  }

  /** How the send `late` is about ended, or `undefined` when it is about none that may have run. */
  late(late: Late): Ended | undefined {
    const text = this.#unsure.get(late.request);
    if (text === undefined) {
      return undefined;
    }
    if (late.settled === "accepted" && this.#out?.request === late.request) {
      this.#out.accepted = true;
    }
    return this.#settle(text, late.request, late.settled);
  }

  #settle(text: string, request: RequestId, sent: Sent): Ended {
    const next = after(sent, text, request);
    if (next.retry === undefined) {
      this.#unsure.delete(request);
    } else {
      this.#unsure.set(request, text);
    }
    const latest = this.#latest?.request === request;
    if (latest) {
      this.#retry = next.retry;
    }
    return { text, taken: next.text === "", latest, problem: next.problem };
  }
}
