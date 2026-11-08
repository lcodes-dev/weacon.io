import type { Envelope, RunEvent, RunId } from "./protocol.ts";
import { isRunEvent } from "./transcript.ts";

/**
 * Gathers the events of the first run the server streams, and hands them over in batches, at
 * most one per `schedule`d call, so a burst of messages costs one render.
 */
export class Feed {
  #run: RunId | undefined;
  #pending: RunEvent[] = [];
  #schedule: (flush: () => void) => void;
  #deliver: (events: RunEvent[]) => void;

  constructor(schedule: (flush: () => void) => void, deliver: (events: RunEvent[]) => void) {
    this.#schedule = schedule;
    this.#deliver = deliver;
  }

  push(envelope: Envelope): void {
    // The server's frames are parsed here, where they arrive; a malformed one is dropped.
    if (envelope.type !== "run" || typeof envelope.run !== "string" || !isRunEvent(envelope.event)) {
      return;
    }
    this.#run ??= envelope.run;
    if (envelope.run !== this.#run) {
      return;
    }
    if (this.#pending.length === 0) {
      this.#schedule(() => {
        const events = this.#pending;
        this.#pending = [];
        this.#deliver(events);
      });
    }
    this.#pending.push(envelope.event);
  }
}
