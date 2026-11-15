import { PROTOCOL } from "../../../tooling/ts/version.ts";
import { type Incoming, parse } from "./incoming.ts";
import type { App, Envelope } from "./protocol.ts";

/** The native host's name, as `weacon connect-chrome` registers it. */
export const HOST = "io.weacon.core";

const DEADLINE_MS = 30_000;

/** A failure named by a dotted code, the only part of it that reaches the core's log. */
export class Failure extends Error {
  readonly code: string;

  constructor(code: string) {
    super(code);
    this.code = code;
  }
}

/** The part of a native-messaging port this file uses; `chrome.runtime.Port` has it. */
export type Port = {
  postMessage(message: Envelope): void;
  disconnect(): void;
  onMessage: { addListener(listener: (message: unknown) => void): void };
  onDisconnect: { addListener(listener: () => void): void };
};

/** A greeted conversation with the core. */
export type Link = {
  /** A new envelope id. */
  next(): number;
  /** Sends `envelope` and waits `deadline` ms at most for its final reply. */
  ask(envelope: Envelope, deadline?: number): Promise<Incoming>;
  /** Sends `envelope`; any reply goes to the link's listener. */
  send(envelope: Envelope): void;
  close(): void;
};

/**
 * Greets the core over `port` as `me`. What no `ask` waits for goes to `heard`, and `closed` is
 * called once the port closes.
 */
export async function connect(
  port: Port,
  me: App,
  heard: (message: Incoming) => void,
  closed: () => void,
  deadline = DEADLINE_MS,
): Promise<Link> {
  const link = conversation(port, heard, closed);
  try {
    await greet(link, me, deadline);
  } catch (error: unknown) {
    link.close();
    throw error;
  }
  return link;
}

/** Tells the core about a failure by its code, over `port`, which it then closes. */
export async function report(port: Port, me: App, code: string, deadline = DEADLINE_MS): Promise<void> {
  const link = conversation(port, () => {}, () => {});
  try {
    await greet(link, me, deadline);
    link.send({ id: link.next(), type: "error", code, retryable: false });
  } catch {
    // With no core to tell, there is nowhere else to send it.
  } finally {
    link.close();
  }
}

async function greet(link: Link, me: App, deadline: number): Promise<void> {
  const reply = await link.ask({ id: link.next(), type: "hello", protocol: PROTOCOL, app: me }, deadline);
  if (reply.type !== "welcome") {
    const other = reply.type === "error" && reply.code === "protocol.unsupported";
    throw new Failure(other ? "extension.core_other_protocol" : "extension.core_refused");
  }
}

// Replies wait by envelope id, skipping an `accepted` receipt; the port closing fails every
// wait, and so does the deadline passing without a reply.
function conversation(port: Port, heard: (message: Incoming) => void, closed: () => void): Link {
  const waiting = new Map<number, (reply: Incoming | Failure) => void>();
  let open = true;
  let ids = 0;
  port.onMessage.addListener((message) => {
    const incoming = parse(message);
    if (incoming === undefined || (incoming.type === "receipt" && incoming.state === "accepted")) {
      return;
    }
    const answer = incoming.id === 0 ? undefined : waiting.get(incoming.id);
    return answer === undefined ? heard(incoming) : answer(incoming);
  });
  port.onDisconnect.addListener(() => {
    open = false;
    waiting.forEach((answer) => answer(new Failure("extension.core_unreachable")));
    closed();
  });
  const send = (envelope: Envelope) => {
    if (!open) {
      throw new Failure("extension.core_unreachable");
    }
    port.postMessage(envelope);
  };
  return {
    next: () => (ids += 1),
    send,
    ask: (envelope, deadline = DEADLINE_MS) =>
      new Promise((resolve, reject) => {
        const timer = setTimeout(() => answer(new Failure("extension.core_no_reply")), deadline);
        const answer = (reply: Incoming | Failure) => {
          clearTimeout(timer);
          waiting.delete(envelope.id);
          return reply instanceof Failure ? reject(reply) : resolve(reply);
        };
        waiting.set(envelope.id, answer);
        try {
          send(envelope);
        } catch (error: unknown) {
          answer(error instanceof Failure ? error : new Failure("extension.core_unreachable"));
        }
      }),
    close: () => {
      open = false;
      port.disconnect();
      waiting.forEach((answer) => answer(new Failure("extension.core_unreachable")));
    },
  };
}
