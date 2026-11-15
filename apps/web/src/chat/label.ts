import type { AgentRef, MemberId } from "../protocol.ts";

const AGENTS = new Map([
  ["claude", "Claude Code"],
  ["codex", "Codex"],
]);

/** Each member's name, by their id. */
export type Names = ReadonlyMap<MemberId, string>;

const NO_NAMES: Names = new Map();

/**
 * What an agent's messages are labelled, such as "ada's Claude Code · laptop" or "Shared Codex · team
 * VPS": its owner is `agent`'s member, or else `owner`, and the parts that aren't known are left out,
 * down to "Agent".
 */
export function label(agent: AgentRef | undefined, names: Names = NO_NAMES, owner?: MemberId): string {
  const vendor = AGENTS.get(agent?.kind ?? "") ?? "Agent";
  const member = agent?.owner.kind === "member" ? agent.owner.member : owner;
  const name = member === undefined || agent?.owner.kind === "shared" ? undefined : names.get(member);
  const who = agent?.owner.kind === "shared" ? `Shared ${vendor}` : name === undefined ? vendor : `${name}'s ${vendor === "Agent" ? "agent" : vendor}`;
  return agent?.machine === undefined ? who : `${who} · ${agent.machine}`;
}
