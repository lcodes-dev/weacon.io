import type { ChatRef } from "../chat-ref.ts";
import type { ChatAction, Envelope, RequestId } from "../protocol.ts";
import type { Receipts, Settled } from "../receipts.ts";

/** A command to a chat. */
export type Command = Extract<Envelope, { type: "chat_command" }>;

/** The command that asks `action` of the chat at `chatRef`, under `request`. */
export function chatCommand(id: number, chatRef: ChatRef, request: RequestId, action: ChatAction): Command {
  return { id, type: "chat_command", ...chatRef, request, action };
}

/** Sends `command` through `send` and waits for its receipt. */
export function deliver(command: Command, receipts: Receipts, send: (envelope: Envelope) => void): Promise<Settled> {
  const settled = receipts.wait(command.request, command.id);
  try {
    send(command);
  } catch {
    // A retry then waits afresh, under its own envelope and deadline.
    receipts.settle(command.request, "lost");
  }
  return settled;
}
