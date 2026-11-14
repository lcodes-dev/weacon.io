import type { MachineSummary, PairPending } from "../protocol.ts";
import type { Pins } from "./pins.ts";

/**
 * Pins the key of a machine the server has just paired, if it is the one the person checked and
 * no other key is pinned for it; otherwise says why not. A store that fails pins nothing.
 */
export async function keep(pins: Pins, machine: MachineSummary, shown: PairPending): Promise<string | undefined> {
  let before: string | undefined;
  try {
    before = await pins.pinned(machine.id);
  } catch {
    return "This browser couldn't check the machine's key. It's paired, but its terminal won't open here.";
  }
  if (machine.key !== shown.key || (before !== undefined && before !== machine.key)) {
    return "The machine's key isn't the one you checked. If it shows in your list, unpair it.";
  }
  try {
    await pins.pin(machine.id, machine.key);
  } catch {
    return "This browser couldn't keep the machine's key. It's paired, but its terminal won't open here.";
  }
  return undefined;
}
