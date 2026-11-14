import { PROTOCOL } from "../../../tooling/ts/version.ts";
import type { App, Envelope, Pick } from "./protocol.ts";

/** The native host's name, as `weacon connect-chrome` registers it. */
export const HOST = "io.weacon.core";

const DEADLINE_MS = 30_000;

declare const WEACON_VERSION: string;
declare const WEACON_COMMIT: string;

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

/** How the extension introduces itself; `tools/build.ts` defines both values. */
export function app(): App {
  return { name: "weacon-extension", version: WEACON_VERSION, commit: WEACON_COMMIT };
}

/** The message that carries `pick`, as the core receives it. */
export function pickEnvelope(pick: Pick): Envelope {
  return { id: 2, type: "pick", ...pick };
}

/** Sends `pick` to the core over `port`, which it then closes; each reply has `deadline` ms. */
export async function deliver(port: Port, me: App, pick: Pick, deadline = DEADLINE_MS): Promise<void> {
  const ask = conversation(port, deadline);
  try {
    await greet(ask, me, "element.pick");
    const reply = await ask(pickEnvelope(pick));
    if (reply.type === "receipt" && reply.state === "completed") {
      return;
    }
    throw new Failure(
      (reply.type === "receipt" && reply.state === "failed") || reply.type === "error"
        ? reply.code
        : "extension.unexpected_reply",
    );
  } finally {
    port.disconnect();
  }
}

/** Tells the core about a failure by its code, over `port`, which it then closes. */
export async function report(port: Port, me: App, code: string, deadline = DEADLINE_MS): Promise<void> {
  try {
    await greet(conversation(port, deadline), me);
    port.postMessage({ id: 2, type: "error", code, retryable: false });
  } catch {
    // With no core to tell, there is nowhere else to send it.
  } finally {
    port.disconnect();
  }
}

async function greet(ask: Ask, me: App, capability?: string): Promise<void> {
  const reply = await ask({ id: 1, type: "hello", protocol: PROTOCOL, app: me });
  if (reply.type !== "welcome") {
    const other = reply.type === "error" && reply.code === "protocol.unsupported";
    throw new Failure(other ? "extension.core_other_protocol" : "extension.core_refused");
  }
  if (capability !== undefined && !(reply.capabilities ?? []).includes(capability)) {
    throw new Failure("extension.core_too_old");
  }
}

type Ask = (envelope: Envelope) => Promise<Envelope>;

// Sends a request and waits for its final reply, skipping an `accepted` receipt; the port
// closing fails every wait, and so does `deadline` ms passing without a reply.
function conversation(port: Port, deadline: number): Ask {
  const waiting = new Map<number, (reply: Envelope | Failure) => void>();
  let open = true;
  port.onMessage.addListener((message) => {
    if (isEnvelope(message) && !(message.type === "receipt" && message.state === "accepted")) {
      waiting.get(message.id)?.(message);
    }
  });
  port.onDisconnect.addListener(() => {
    open = false;
    waiting.forEach((answer) => answer(new Failure("extension.core_unreachable")));
  });
  return (envelope) =>
    new Promise((resolve, reject) => {
      const timer = setTimeout(() => answer(new Failure("extension.core_no_reply")), deadline);
      const answer = (reply: Envelope | Failure) => {
        clearTimeout(timer);
        waiting.delete(envelope.id);
        return reply instanceof Failure ? reject(reply) : resolve(reply);
      };
      if (!open) {
        return answer(new Failure("extension.core_unreachable"));
      }
      waiting.set(envelope.id, answer);
      port.postMessage(envelope);
    });
}

// The host relays only what parsed as an envelope; a newer core may still send types this
// build doesn't know, which the callers treat as unexpected.
function isEnvelope(message: unknown): message is Envelope {
  return (
    typeof message === "object" &&
    message !== null &&
    "id" in message &&
    typeof message.id === "number" &&
    "type" in message &&
    typeof message.type === "string"
  );
}
