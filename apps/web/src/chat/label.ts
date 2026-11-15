import type { AgentRef } from "../protocol.ts";

const AGENTS = new Map([
  ["claude", "Claude Code"],
  ["codex", "Codex"],
]);

/** What `kind` of agent is called, such as "Codex"; "Agent" for one this app doesn't know. */
export function vendorName(kind: string | undefined): string {
  return AGENTS.get(kind ?? "") ?? "Agent";
}

/**
 * What an agent's messages are labelled, such as "Shared Codex · team VPS": the parts that aren't
 * known are left out, down to "Agent". No message names a member, so a member's agent goes unnamed.
 */
export function label(agent: AgentRef | undefined): string {
  const vendor = vendorName(agent?.kind);
  const who = agent?.owner.kind === "shared" ? `Shared ${vendor}` : vendor;
  return agent?.machine === undefined ? who : `${who} · ${agent.machine}`;
}
