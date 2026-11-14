import type { Command } from "../composer/prompt.ts";
import type { Place } from "../place.ts";
import type { RequestId, RunId } from "../protocol.ts";

/** The command that answers the prompt `prompt` of `run`, allowing it or not. */
export function answer(id: number, place: Place, request: RequestId, run: RunId, prompt: number, allow: boolean): Command {
  return { id, type: "chat_command", ...place, request, action: { kind: "permission", answer: { run, prompt, allow } } };
}

/** The command that stops the chat's turn that is running. */
export function interrupt(id: number, place: Place, request: RequestId): Command {
  return { id, type: "chat_command", ...place, request, action: { kind: "interrupt" } };
}
