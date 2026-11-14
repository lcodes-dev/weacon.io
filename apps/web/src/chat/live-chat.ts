import type { ChatRef } from "../chat-ref.ts";
import type { ChatMessage, ErrorMessage, RunMessage } from "../incoming.ts";
import type { Envelope, RunId } from "../protocol.ts";
import { answered } from "./chat.ts";
import { heard } from "./heard.ts";
import { START, type Step, type Sync, ended, hear, more, open } from "./sync.ts";

/** What a live chat needs from the page: envelope ids, a way to send, and a frame to render on. */
export type Wiring = {
  id: () => number;
  send: (envelope: Envelope) => void;
  schedule: (flush: () => void) => void;
  show: (sync: Sync) => void;
};

/**
 * Keeps the chat at `ref` in step with the server, and shows it at most once per scheduled
 * flush, so a burst of messages costs one render.
 */
export class LiveChat {
  #ref: ChatRef;
  #wiring: Wiring;
  #sync: Sync = START;
  #pending = false;

  constructor(ref: ChatRef, wiring: Wiring) {
    this.#ref = ref;
    this.#wiring = wiring;
  }

  /** Asks for the chat afresh, as once the connection is live. */
  open(): void {
    this.#take(open(this.#sync, this.#ref, this.#wiring.id));
  }

  /** Asks for older events, as when the reader wants to see them. */
  more(): void {
    this.#take(more(this.#sync, this.#ref, this.#wiring.id));
  }

  /** Stops the prompt `prompt` of `run` being answerable, as once its answer is taken. */
  answered(run: RunId, prompt: number): void {
    const { chat } = this.#sync;
    if (chat !== undefined) {
      this.#change({ ...this.#sync, chat: answered(chat, run, prompt) });
    }
  }

  /** Stops waiting for answers, keeping the chat, as when the connection ends. */
  end(): void {
    this.#change(ended(this.#sync));
  }

  /** Hears `message`, when it is about this chat. */
  push(message: ChatMessage | RunMessage | ErrorMessage): void {
    const about = heard(message, this.#ref);
    if (about !== undefined) {
      this.#take(hear(this.#sync, about, this.#ref, this.#wiring.id));
    }
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
    if (!this.#pending) {
      this.#pending = true;
      this.#wiring.schedule(() => {
        // A flush that runs twice, by frame and by timer, shows the chat once.
        if (this.#pending) {
          this.#pending = false;
          this.#wiring.show(this.#sync);
        }
      });
    }
  }
}

/**
 * A `schedule` for `LiveChat` that flushes on the next frame. A tab hidden before then gets no frame,
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
