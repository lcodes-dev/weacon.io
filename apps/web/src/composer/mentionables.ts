import { vendorName } from "../chat/label.ts";
import type { AgentKind, MachineId, MachineName } from "../protocol.ts";
import type { MentionItem, Query } from "./draft.ts";

/**
 * The agent of `kind` on `machine`, as a message mentions it to call it: the writer's own under
 * its vendor's name, and one on a machine shared with the workspace, as `shared` names it, as
 * the workspace's.
 */
export function agentItem(machine: MachineId, kind: AgentKind, shared?: MachineName): MentionItem {
  const vendor = vendorName(kind);
  return shared === undefined ? { id: machine, label: vendor, note: "Your agent" } : { id: machine, label: `Shared ${vendor} · ${shared}`, note: "Shared agent" };
}

/** `agents`, each on the machine `machines` names for it, with that name on those that would otherwise read alike. */
export function told(agents: readonly MentionItem[], machines: ReadonlyMap<MachineId, MachineName>): MentionItem[] {
  const alike = (one: MentionItem) => agents.filter(({ label }) => label === one.label).length > 1;
  return agents.map((one) => (alike(one) ? { ...one, label: `${one.label} · ${machines.get(one.id) ?? one.id}` } : one));
}

/**
 * What the picker offers for the query `at`: those of `agents`, the ones the writer may call in
 * the chat, with a word of their name that starts with the query, ignoring case. Nothing once it
 * was dismissed, or while a message is being sent, as a pick then would be lost when the box
 * clears.
 */
export function offered(at: Query | undefined, dismissed: number | undefined, sending: boolean, agents: readonly MentionItem[]): MentionItem[] {
  if (at === undefined || at.start === dismissed || sending) {
    return [];
  }
  const wanted = at.text.toLocaleLowerCase();
  const named = ({ label }: MentionItem) => label.toLocaleLowerCase().split(/\s+/u);
  return agents.filter((agent) => named(agent).some((word) => word.startsWith(wanted)));
}
