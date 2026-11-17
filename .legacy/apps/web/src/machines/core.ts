import type { MachineSummary } from "../protocol.ts";

/** Whether `machine`'s core said it opens terminals, as one where there is a desktop terminal does. */
export function opensTerminals(machine: MachineSummary): boolean {
  return (machine.offers ?? []).includes("worktree.open_terminal");
}
