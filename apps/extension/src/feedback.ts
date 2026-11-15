const SAID: Record<string, string> = {
  "extension.page_not_web": "Weacon picks elements on web pages only.",
  "extension.page_not_allowed": "Click Weacon's button on this page, then pick again.",
  "extension.element_hidden": "That element isn't on the screen.",
  "extension.pick_too_large": "The element is too large to send.",
  "extension.core_unreachable": "Weacon isn't running on this machine, or Chrome isn't connected to it.",
  "extension.core_no_reply": "Weacon on this machine didn't answer.",
  "extension.core_too_old": "Update Weacon on this machine.",
  "extension.core_other_protocol": "Update Weacon on this machine.",
  "extension.core_refused": "Weacon on this machine didn't let the extension in.",
  "chat.empty_prompt": "Write something to send.",
  "pick.missing": "The element couldn't be sent. Pick it again.",
  "pick.not_kept": "The element couldn't be sent. Pick it again.",
  "run.interrupted": "Stopped.",
};

/** What the panel says after something failed with `code`. */
export function say(code: string): string {
  return SAID[code] ?? `Something went wrong (${code}).`;
}

/** Whether to tell the core about `code`: codes from the core are already in its log. */
export function reported(code: string): boolean {
  return code.startsWith("extension.");
}
