const TITLES: Record<string, string> = {
  "extension.page_not_local": "Weacon picks elements on local pages only",
  "extension.core_unreachable": "Weacon's core isn't running, or Chrome isn't connected to it",
  "extension.core_no_reply": "Weacon's core didn't answer",
  "extension.core_too_old": "Update Weacon's core to pick elements",
  "extension.pick_too_large": "The element is too large to send",
};

/** What the toolbar button says after a pick failed with `code`. */
export function title(code: string): string {
  return TITLES[code] ?? "The element couldn't be sent to Weacon";
}

/** Whether to tell the core about `code`: codes from the core are already in its log. */
export function reported(code: string): boolean {
  return code.startsWith("extension.");
}
