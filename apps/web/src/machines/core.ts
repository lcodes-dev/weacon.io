import type { MachineSummary } from "../protocol.ts";

/** The machine's weacon version, as the list shows it after its other details; nothing before its core has connected. */
export function coreVersion(machine: MachineSummary): string {
  return machine.core === undefined ? "" : `, weacon ${machine.core.version}`;
}

/** Whether the machine's core is older than the server supports, so some features don't reach it. */
export function isOutdated(machine: MachineSummary): boolean {
  return (machine.missing ?? []).length > 0;
}
