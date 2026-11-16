import { statusText } from "../connection.ts";
import type { Code } from "../protocol.ts";
import { UNREACHABLE } from "./api.ts";

const PROBLEMS = new Map<Code, string>([
  [UNREACHABLE, statusText({ state: "ended", code: UNREACHABLE })],
  ["sign_in.failed", "That code didn't work. Check it, or ask for a new one."],
  ["sign_in.too_many", "Too many tries. Wait a while, then try again."],
  ["workspace.name_invalid", "Give it a name of one short line."],
  ["workspace.too_many", "You can't make more workspaces."],
  ["email.invalid", "That isn't an email address."],
  ["member.not_allowed", "You can't do that in this workspace."],
  ["member.not_found", "That person isn't here any more."],
  ["member.exists", "That address is a member already."],
  ["invitation.exists", "That address is invited already."],
  ["invitation.not_found", "That invitation has ended."],
  ["pairing.not_found", "No machine is waiting under that code. Check it, or run weacon pair again."],
  ["pairing.too_many", "Too many tries. Wait a while, then try again."],
  ["machine.not_found", "That machine isn't paired any more."],
  ["workspace.reconnecting", "The workspace is reconnecting. Try again in a moment."],
  ["machine.offline", "This machine isn't connected. Start Weacon on it, then try again."],
  ["machine.no_answer", "The machine didn't answer. Try again."],
  ["request.unknown", "This machine's weacon is too old for this. Update it, then try again."],
  ["symbols.still_reading", "The code is still being read. Try again shortly."],
  ["relay.busy", "The server is busy. Try again in a moment."],
  ["settings.not_owner", "Only this machine's owner can see or change its settings."],
  ["sessions.not_owner", "Only this machine's owner can see its sessions."],
  ["session.missing", "This session isn't on the machine any more."],
  ["task.invalid", "Give it a title of one short line."],
  ["task.stale", "This task changed since you read it. Check it, then try again."],
  ["task.not_found", "That task isn't there any more."],
  ["doc.invalid", "Give the doc a title of one short line."],
  ["doc.stale", "Someone saved this doc since you read it."],
  ["doc.not_found", "That doc isn't there any more."],
  ["doc.too_large", "The doc is too long to save."],
  ["doc.unpinned", "Links to code can only be added from a machine."],
  ["doc.unlinked", "Links to code can only be changed from a machine."],
  ["proposal.closed", "This proposal was already decided or withdrawn."],
  ["proposal.not_found", "That proposal isn't there any more."],
]);

/** What the page says when a request fails with `code`; codes it doesn't know get a general line. */
export function problemText(code: Code): string {
  return PROBLEMS.get(code) ?? "Something went wrong. Try again.";
}
