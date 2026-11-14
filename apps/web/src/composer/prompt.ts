import { type Status, offers } from "../connection.ts";
import type { Place } from "../place.ts";
import type { Envelope, RequestId } from "../protocol.ts";
import type { Receipts, Settled } from "../receipts.ts";

/** How sending a prompt ended: as its command did, or refused here for its size. */
export type Sent = Settled | "too_long";

/** Sends `text` under `request`. */
export type Send = (text: string, request: RequestId) => Promise<Sent>;

// The server's limit on one message from a browser, in bytes: `MAX_MESSAGE` in its feed.
const FEED_LIMIT = 16_384;

/** A command to a chat. */
export type Command = Extract<Envelope, { type: "chat_command" }>;

/** The command that sends `text` to the chat at `place`, under `request`. */
export function prompt(id: number, place: Place, request: RequestId, text: string): Command {
  return { id, type: "chat_command", ...place, request, action: { kind: "prompt", text } };
}

/** Sends `command` through `send` and waits for its receipt, unless it is too long to send. */
export function deliver(command: Command, receipts: Receipts, send: (envelope: Envelope) => void): Promise<Sent> {
  if (new TextEncoder().encode(JSON.stringify(command)).length > FEED_LIMIT) {
    return Promise.resolve("too_long");
  }
  const settled = receipts.wait(command.request, command.id);
  try {
    send(command);
  } catch {
    // A retry then waits afresh, under its own envelope and deadline.
    receipts.settle(command.request, "lost");
  }
  return settled;
}

/** A prompt that may have run, kept with its request so sending it again can't run it twice. */
export type Retry = { text: string; request: RequestId };

/** The request to send `text` under: the retry's when it is the same text, else `fresh`. */
export function requestFor(retry: Retry | undefined, text: string, fresh: () => RequestId): RequestId {
  return retry?.text === text ? retry.request : fresh();
}

const PROBLEMS: Record<Exclude<Sent, "accepted">, string> = {
  too_long: "Too long to send; shorten it and try again",
  refused: "The message wasn't sent",
  lost: "The connection closed; the message may not have been sent",
  unsure: "The server didn't answer; the message may not have been sent",
};

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
  const retry = sent === "lost" || sent === "unsure" ? { text, request } : undefined;
  return { text, problem: PROBLEMS[sent], retry };
}

/** Why the box can't send to `place` now, or `undefined` when it can. */
export function blocked(status: Status, place: Place | undefined): string | undefined {
  if (place === undefined) {
    return "Pick a chat to send";
  }
  if (status.state !== "live") {
    return "Messages send once the server is connected";
  }
  return offers(status, "chat.send") ? undefined : "This server can't take messages yet";
}

/** A request id made of `bytes` in hex. */
export function requestId(bytes: Uint8Array): RequestId {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}
