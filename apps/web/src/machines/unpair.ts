import type { MachineId } from "../protocol.ts";
import type { Answer } from "../shell/api.ts";

/** Unpairs `machine`; one already unpaired elsewhere is as gone as one unpaired here. */
export async function unpair(api: { unpair(machine: MachineId): Promise<Answer<undefined>> }, machine: MachineId): Promise<Answer<undefined>> {
  const answer = await api.unpair(machine);
  return !answer.ok && answer.code === "machine.not_found" ? { ok: true, value: undefined } : answer;
}
