import { type Command, deliver } from "../chat/commands.ts";
import type { ChatRef } from "../chat-ref.ts";
import type { Envelope, RequestId } from "../protocol.ts";
import { type Receipts, type Settled, mayHaveRun, settledText } from "../receipts.ts";

/** How sending a prompt ended: as its command did, or refused here for its size. */
export type Sent = Settled | "too_long";

/** Sends `text` under `request`. */
export type Send = (text: string, request: RequestId) => Promise<Sent>;

// The server's limit on one message from a browser, in bytes: `MAX_MESSAGE` in its feed.
const FEED_LIMIT = 16_384;

/** Sends the prompt `command` through `send` and waits for its receipt, unless it is too long to send. */
export function deliverPrompt(command: Command, receipts: Receipts, send: (envelope: Envelope) => void): Promise<Sent> {
  if (new TextEncoder().encode(JSON.stringify(command)).length > FEED_LIMIT) {
    return Promise.resolve("too_long");
  }
  return deliver(command, receipts, send);
}

/** A prompt that may have run, kept with its request so sending it again can't run it twice. */
export type Retry = { text: string; request: RequestId };

const MESSAGE = { the: "the message", not: "wasn't sent", maybe: "may not have been sent" };

/** What the box holds after a send. */
export type After = { text: string; problem: string | undefined; retry: Retry | undefined };

/**
 * What the box holds once sending `text` under `request` ended as `sent`: empty when accepted,
 * else the text, why, and the request to retry it under when it may have run.
 */
export function after(sent: Sent, text: string, request: RequestId): After {
  if (sent === "accepted") {
    return { text: "", problem: undefined, retry: undefined };
  }
  const retry = sent !== "too_long" && mayHaveRun(sent) ? { text, request } : undefined;
  const problem = sent === "too_long" ? "It's too long to send. Shorten it and try again." : settledText(sent, MESSAGE);
  return { text, problem, retry };
}

/** How a command ended, heard after its wait ran out. */
export type Late = { request: RequestId; settled: Settled };

/** Hands `listener` each late receipt until the returned function is called, so none is lost in a batch. */
export type Lates = (listener: (late: Late) => void) => () => void;

/** Late receipts as `Lates`: each one `hear` takes reaches every listener at once. */
export function lateFeed(): { lates: Lates; hear: (late: Late) => void } {
  const listeners = new Set<(late: Late) => void>();
  return {
    lates: (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    hear: (late) => [...listeners].forEach((listener) => listener(late)),
  };
}

/** Why the box can't send to `chatRef` now, or `undefined` when it can. */
export function blocked(live: boolean, chatRef: ChatRef | undefined, sends: boolean): string | undefined {
  if (chatRef === undefined) {
    return "Pick a chat to send to.";
  }
  if (!live) {
    return "Messages send once the server is connected.";
  }
  return sends ? undefined : "This server can't take messages yet.";
}
