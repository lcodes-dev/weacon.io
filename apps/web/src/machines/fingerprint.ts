import { hex } from "../hex.ts";
import type { MachineKey } from "../protocol.ts";

/** Whether `key` is 64 lowercase hex digits, the only form a machine key takes. */
export function isMachineKey(key: string): boolean {
  return /^[0-9a-f]{64}$/.test(key);
}

/**
 * What a person compares with their terminal: the first 16 bytes of the SHA-256 of the
 * machine's 32-byte key, as 8 groups of 4 lowercase hex digits. The core prints the same.
 */
export async function fingerprint(key: MachineKey): Promise<string> {
  if (!isMachineKey(key)) {
    throw new RangeError("not a machine key");
  }
  const bytes = Uint8Array.from(key.match(/../g) ?? [], (pair) => Number.parseInt(pair, 16));
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", bytes));
  return Array.from({ length: 8 }, (_, group) => hex(digest.subarray(2 * group, 2 * group + 2))).join(" ");
}
