import type { MachineOs } from "../protocol.ts";

const NAMES = new Map<string, string>([
  ["macos", "macOS"],
  ["linux", "Linux"],
  ["windows", "Windows"],
]);

/** How a machine's system reads on the page; one a newer server knows reads as another system. */
export function osName(os: MachineOs | string): string {
  return NAMES.get(os) ?? "Another system";
}
