import { type Status, offers } from "../connection.ts";
import type { ChatRef } from "../chat-ref.ts";
import type { ChatAction, Envelope, RequestId } from "../protocol.ts";
import type { Receipts, Settled } from "../receipts.ts";

/** How sending a prompt ended: as its command did, or refused here for its size. */
export type Sent = Settled | "too_long";

/** Sends `text` under `request`. */
export type Send = (text: string, request: RequestId) => Promise<Sent>;

// The server's limit on one message from a browser, in bytes: `MAX_MESSAGE` in its feed.
const FEED_LIMIT = 16_384;

/** A command to a chat. */
export type Command = Extract<Envelope, { type: "chat_command" }>;

/** The command that asks `action` of the chat at `chatRef`, under `request`. */
export function chatCommand(id: number, chatRef: ChatRef, request: RequestId, action: ChatAction): Command {
  return { id, type: "chat_command", ...chatRef, request, action };
}

/** The command that sends `text` to the chat at `chatRef`, under `request`. */
export function prompt(id: number, chatRef: ChatRef, request: RequestId, text: string): Command {
  return chatCommand(id, chatRef, request, { kind: "prompt", text });
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
  too_long: "It's too long to send. Shorten it and try again.",
  refused: "The message wasn't sent.",
  lost: "The connection closed. The message may not have been sent.",
  unsure: "The server didn't answer. The message may not have been sent.",
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

/** How a command ended, heard after its wait ran out. */
export type Late = { request: RequestId; settled: Settled };

/**
 * What the box holds once `late` says how the send it may retry ended, or `undefined` when `late`
 * is about another send. Text edited since is kept.
 */
export function afterLate(retry: Retry | undefined, text: string, late: Late): After | undefined {
  if (retry?.request !== late.request) {
    return undefined;
  }
  const ended = after(late.settled, retry.text, retry.request);
  return text === retry.text ? ended : { ...ended, text };
}

/** Why the box can't send to `chatRef` now, or `undefined` when it can. */
export function blocked(status: Status, chatRef: ChatRef | undefined): string | undefined {
  if (chatRef === undefined) {
    return "Pick a chat to send to.";
  }
  if (status.state !== "live") {
    return "Messages send once the server is connected.";
  }
  return offers(status, "chat.send") ? undefined : "This server can't take messages yet.";
}
