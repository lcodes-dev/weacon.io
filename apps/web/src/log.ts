import type { ClientLog, Envelope } from "./protocol.ts";

/** Every event the web app logs: fixed names, so no content can reach the server's log. */
export type Event = "app started" | "connection lost" | "connection refused";

/** Every failure the web app reports, by its code alone. */
export type Failure = "web.crashed";

/** Ids a record may carry; nothing else can be attached. */
export type Ids = Pick<ClientLog, "code">;

// Records kept while there is no server to send them to; the oldest go first.
const KEEP = 200;

/** Forwards logs and error reports to the server, holding them until it can take them. */
export class Log {
  #held: Envelope[] = [];
  #send: ((envelope: Envelope) => void) | undefined;
  #forwards = false;

  info(event: Event, ids: Ids = {}): void {
    this.#record({ id: 0, type: "log", level: "info", msg: event, ...ids });
  }

  warn(event: Event, ids: Ids = {}): void {
    this.#record({ id: 0, type: "log", level: "warn", msg: event, ...ids });
  }

  /** Reports a failure; the server takes an `error` from a client as its report. */
  report(code: Failure): void {
    this.#record({ id: 0, type: "error", code, retryable: false });
  }

  /** Sends what's held, and from now on each record, through `send`; logs only if `forwards`. */
  attach(send: (envelope: Envelope) => void, forwards: boolean): void {
    this.#send = send;
    this.#forwards = forwards;
    const held = this.#held;
    this.#held = [];
    held.forEach((envelope) => this.#record(envelope));
  }

  /** Holds records again, as when the connection ends. */
  detach(): void {
    this.#send = undefined;
  }

  #record(envelope: Envelope): void {
    if (this.#send === undefined) {
      this.#held = [...this.#held, envelope].slice(-KEEP);
    } else if (envelope.type !== "log" || this.#forwards) {
      this.#send(envelope);
    }
  }
}
