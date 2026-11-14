import { statusText } from "../connection.ts";
import type { Code } from "../protocol.ts";
import { UNREACHABLE } from "./api.ts";

const PROBLEMS = new Map<Code, string>([
  [UNREACHABLE, statusText({ state: "ended", code: UNREACHABLE })],
  ["sign_in.failed", "That code didn't work. Check it, or ask for a new one."],
  ["sign_in.too_many", "Too many tries. Wait a while, then try again."],
  ["workspace.name_invalid", "Give it a name of one short line."],
  ["workspace.too_many", "You can't make more workspaces."],
  ["pairing.not_found", "No machine is waiting under that code. Check it, or run weacon pair again."],
  ["pairing.too_many", "Too many tries. Wait a while, then try again."],
  ["machine.not_found", "That machine isn't paired any more."],
  ["workspace.reconnecting", "The workspace is reconnecting. Try again in a moment."],
  ["machine.offline", "This machine isn't connected. Start Weacon on it, then try again."],
  ["machine.no_answer", "The machine didn't answer. Try again."],
  ["relay.busy", "The server is busy. Try again in a moment."],
  ["settings.not_owner", "Only this machine's owner can see or change its settings."],
]);

/** What the page says when a request fails with `code`; codes it doesn't know get a general line. */
export function problemText(code: Code): string {
  return PROBLEMS.get(code) ?? "Something went wrong. Try again.";
}
