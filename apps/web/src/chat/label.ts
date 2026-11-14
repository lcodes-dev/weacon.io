/** Whose agent a run is: a member's own, or the workspace's shared one. */
export type Owner = { kind: "member"; member: string } | { kind: "shared" };

/** Who runs a run, as far as its chat's log says. */
export type RunAgent = { kind?: string; owner?: Owner; machine?: string };

/** The names the screen knows, by member id and machine id. */
export type Names = { members: ReadonlyMap<string, string>; machines: ReadonlyMap<string, string> };

const AGENTS = new Map([
  ["claude", "Claude"],
  ["codex", "Codex"],
]);

/**
 * What an agent's messages are labelled, such as "Ada's Claude · laptop": the parts that aren't
 * known are left out, down to "Agent".
 */
export function label(agent: RunAgent | undefined, names: Names): string {
  const vendor = AGENTS.get(agent?.kind ?? "") ?? "Agent";
  const owner = agent?.owner;
  const member = owner?.kind === "member" ? names.members.get(owner.member) : undefined;
  const who = owner?.kind === "shared" ? `Shared ${vendor}` : member === undefined ? vendor : `${member}'s ${vendor}`;
  const machine = agent?.machine === undefined ? undefined : names.machines.get(agent.machine);
  return machine === undefined ? who : `${who} · ${machine}`;
}
