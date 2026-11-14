import type { MachineId, MachineKey } from "../protocol.ts";
import { fingerprint } from "./fingerprint.ts";
import type { Pins } from "./pins.ts";

/** What this browser makes of a machine's key: trusted, another key trusted, or one to confirm by its fingerprint. */
export type Checked = { kind: "match" } | { kind: "mismatch" } | { kind: "failed" } | { kind: "unpinned"; key: MachineKey; shown: string };

/** Checks `key` against this browser's pins in one step; any failure is "failed", never a key to confirm. */
export async function checkKey(pins: Pins, machine: MachineId, key: MachineKey): Promise<Checked> {
  try {
    const [found, shown] = await Promise.all([pins.matches(machine, key), fingerprint(key)]);
    return found === "unpinned" ? { kind: "unpinned", key, shown } : found === "match" ? { kind: "match" } : { kind: "mismatch" };
  } catch {
    return { kind: "failed" };
  }
}
