import type { AgentRef } from "../protocol.ts";

const AGENTS = new Map([
  ["claude", "Claude Code"],
  ["codex", "Codex"],
]);

/**
 * What an agent's messages are labelled, such as "Shared Codex · team VPS": the parts that aren't
 * known are left out, down to "Agent". No message names a member, so a member's agent goes unnamed.
 */
export function label(agent: AgentRef | undefined): string {
  const vendor = AGENTS.get(agent?.kind ?? "") ?? "Agent";
  const who = agent?.owner.kind === "shared" ? `Shared ${vendor}` : vendor;
  return agent?.machine === undefined ? who : `${who} · ${agent.machine}`;
}
