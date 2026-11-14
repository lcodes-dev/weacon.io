import type { AgentRef, MemberId } from "../protocol.ts";

const AGENTS = new Map([
  ["claude", "Claude"],
  ["codex", "Codex"],
]);

/**
 * What an agent's messages are labelled, such as "Ada's Claude · laptop", from the members'
 * names the screen knows: the parts that aren't known are left out, down to "Agent".
 */
export function label(agent: AgentRef | undefined, members: ReadonlyMap<MemberId, string>): string {
  const vendor = AGENTS.get(agent?.kind ?? "") ?? "Agent";
  const owner = agent?.owner;
  const member = owner?.kind === "member" ? members.get(owner.member) : undefined;
  const who = owner?.kind === "shared" ? `Shared ${vendor}` : member === undefined ? vendor : `${member}'s ${vendor}`;
  return agent?.machine === undefined ? who : `${who} · ${agent.machine}`;
}
