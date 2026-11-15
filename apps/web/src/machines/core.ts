import type { MachineSummary } from "../protocol.ts";

/** Whether the machine's core left out `capability` of what the server offers, as one too old for it does. */
export function lacks(machine: MachineSummary, capability: string): boolean {
  return (machine.missing ?? []).includes(capability);
}

/** Whether the machine's core is older than the server supports, so some features don't reach it. */
export function isOutdated(machine: MachineSummary): boolean {
  return (machine.missing ?? []).length > 0;
}
