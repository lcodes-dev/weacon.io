import type { Command } from "../composer/prompt.ts";
import type { ChatRef } from "../chat-ref.ts";
import type { RequestId, RunId } from "../protocol.ts";

/** The command that answers the prompt `prompt` of `run`, allowing it or not. */
export function answer(id: number, ref: ChatRef, request: RequestId, run: RunId, prompt: number, allow: boolean): Command {
  return { id, type: "chat_command", ...ref, request, action: { kind: "permission", answer: { run, prompt, allow } } };
}

/** The command that stops the chat's turn that is running. */
export function interrupt(id: number, ref: ChatRef, request: RequestId): Command {
  return { id, type: "chat_command", ...ref, request, action: { kind: "interrupt" } };
}
