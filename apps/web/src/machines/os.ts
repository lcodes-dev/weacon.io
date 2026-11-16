import type { MachineOs } from "../protocol.ts";

const NAMES: Record<MachineOs, string> = {
  macos: "macOS",
  linux: "Linux",
  windows: "Windows",
};

/** How a machine's system reads on the page. */
export function osName(os: MachineOs): string {
  return NAMES[os];
}
