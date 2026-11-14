import type { Place } from "../place.ts";
import type { Envelope } from "../protocol.ts";
import { about } from "./parse.ts";
import { START, type Step, type Sync, ended, hear, more, open } from "./sync.ts";

/** What the feed needs from the page: envelope ids, a way to send, and a frame to render on. */
export type Wiring = {
  id: () => number;
  send: (envelope: Envelope) => void;
  schedule: (flush: () => void) => void;
  show: (sync: Sync) => void;
};

/**
 * Keeps the chat at `place` in step with the server, and shows it at most once per scheduled
 * flush, so a burst of messages costs one render.
 */
export class ChatFeed {
  #place: Place;
  #wiring: Wiring;
  #sync: Sync = START;
  #shown = true;

  constructor(place: Place, wiring: Wiring) {
    this.#place = place;
    this.#wiring = wiring;
  }

  /** Asks for the chat afresh, as once the connection is live. */
  open(): void {
    this.#take(open(this.#sync, this.#place, this.#wiring.id));
  }

  /** Asks for older events, as when the reader wants to see them. */
  more(): void {
    this.#take(more(this.#sync, this.#place, this.#wiring.id));
  }

  /** Stops waiting for answers, keeping the chat, as when the connection ends. */
  end(): void {
    this.#change(ended(this.#sync));
  }

  /** Hears `envelope`; whether it was a chat or run message this feed takes. */
  push(envelope: Envelope): boolean {
    const heard = about(envelope, this.#place);
    if (heard === undefined) {
      return false;
    }
    this.#take(hear(this.#sync, heard, this.#place, this.#wiring.id));
    return true;
  }

  #take(step: Step): void {
    this.#change(step.sync);
    for (const envelope of step.send) {
      this.#wiring.send(envelope);
    }
  }

  #change(sync: Sync): void {
    if (sync === this.#sync) {
      return;
    }
    this.#sync = sync;
    if (this.#shown) {
      this.#shown = false;
      this.#wiring.schedule(() => {
        this.#shown = true;
        this.#wiring.show(this.#sync);
      });
    }
  }
}
