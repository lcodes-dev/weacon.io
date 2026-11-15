import type { MachineId, MachineKey } from "../protocol.ts";
import { fingerprint } from "./fingerprint.ts";
import type { Pins } from "./pins.ts";

/** What this browser made of the machine's key it checked: trusted, another key trusted, or one to confirm by its fingerprint. */
export type Checked =
  | { kind: "match"; key: MachineKey }
  | { kind: "mismatch"; key: MachineKey }
  | { kind: "failed"; key: MachineKey }
  | { kind: "unpinned"; key: MachineKey; shown: string };

/** What a machine's row shows for its key: nothing, a key to confirm, or why it can't be trusted here. */
export type Step = { kind: "none" } | { kind: "confirm"; key: MachineKey; shown: string } | { kind: "failed" } | { kind: "mismatch" } | { kind: "changed" };

/** Checks `key` against this browser's pins in one step; any failure is "failed", never a key to confirm. */
export async function checkKey(pins: Pins, machine: MachineId, key: MachineKey): Promise<Checked> {
  try {
    const [found, shown] = await Promise.all([pins.matches(machine, key), fingerprint(key)]);
    return found === "unpinned" ? { kind: "unpinned", key, shown } : { kind: found, key };
  } catch {
    return { kind: "failed", key };
  }
}

/** The step for `checked`, given the key the server lists now: any other key than the one checked is never trusted or offered. */
export function shownStep(checked: Checked | undefined, listed: MachineKey): Step {
  if (checked === undefined) {
    return { kind: "none" };
  }
  if (checked.key !== listed) {
    return { kind: "changed" };
  }
  if (checked.kind === "unpinned") {
    return { kind: "confirm", key: checked.key, shown: checked.shown };
  }
  return checked.kind === "match" ? { kind: "none" } : checked.kind === "mismatch" ? { kind: "mismatch" } : { kind: "failed" };
}
