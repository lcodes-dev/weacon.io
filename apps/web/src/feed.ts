import type { Envelope, RunEvent, RunId } from "./protocol.ts";
import { isRunEvent } from "./transcript.ts";

/**
 * Gathers the events of the first run the server streams, and hands them over in batches, at
 * most one per `schedule`d call, so a burst of messages costs one render. A batch scheduled
 * more than once goes out on the first call; later ones find nothing to send.
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
        if (events.length === 0) {
          return;
        }
        this.#pending = [];
        this.#deliver(events);
      });
    }
    this.#pending.push(envelope.event);
  }
}

/**
 * A `schedule` for `Feed` that flushes on the next frame. A tab hidden before then gets no frame,
 * so `later` sends the batch instead; a visible tab's batches go out only between renders.
 */
export function nextFrame(
  hidden: () => boolean,
  frame: (run: () => void) => number,
  cancel: (frame: number) => void,
  later: (run: () => void) => void,
): (flush: () => void) => void {
  return (flush) => {
    const pending = frame(flush);
    later(() => {
      if (hidden()) {
        cancel(pending);
        flush();
      }
    });
  };
}
