import type { MachineSummary } from "../protocol.ts";

/** Whether the machine's core is older than the server supports, so some features don't reach it. */
export function isOutdated(machine: MachineSummary): boolean {
  return (machine.missing ?? []).length > 0;
}
