import type { Names } from "../members/names.ts";
import type { AgentKind, AgentRef, MemberId } from "../protocol.ts";

const AGENTS: Record<AgentKind, string> = {
  claude: "Claude Code",
  codex: "Codex",
};

/** What `kind` of agent is called, such as "Codex"; "Agent" when none is named. */
export function vendorName(kind: AgentKind | undefined): string {
  return kind === undefined ? "Agent" : AGENTS[kind];
}

const NO_NAMES: Names = new Map();

/**
 * What an agent's messages are labelled, such as "ada's Claude Code · laptop" or "Shared Codex · team
 * VPS": its owner is `agent`'s member, or else `owner`, and the parts that aren't known are left out,
 * down to "Agent".
 */
export function label(agent: AgentRef | undefined, names: Names = NO_NAMES, owner?: MemberId): string {
  const vendor = vendorName(agent?.kind);
  const where = (who: string) => (agent?.machine === undefined ? who : `${who} · ${agent.machine}`);
  if (agent?.owner.kind === "shared") {
    return where(`Shared ${vendor}`);
  }
  const name = names.get((agent?.owner.kind === "member" ? agent.owner.member : owner) ?? "");
  return where(name === undefined ? vendor : `${name}'s ${vendor === "Agent" ? "agent" : vendor}`);
}
