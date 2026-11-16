import { type Sent, sentProblem } from "../composer/prompt.ts";
import type { AgentKind, MachineId, MachineName, MachineSummary, RepoId, SharedMachine } from "../protocol.ts";
import { lacks } from "../machines/core.ts";
import { type Subject, settledText } from "../receipts.ts";

/** A machine a person may call an agent on: one of their `own`, or one `shared` with the workspace, which may be both. */
export type Callable = { id: MachineId; name: MachineName; own: boolean; shared: boolean };

/** Whether a person can call an agent: on which machines, or what is missing first. */
export type Readiness = { kind: "ready"; machines: Callable[] } | { kind: "unserved" | "unpaired" | "offline" | "outdated" };

/** The machines to call an agent on: the person's own `machines` that run chats, then the connected ones others share with the workspace. */
export function callable(machines: MachineSummary[], shared: SharedMachine[]): Callable[] {
  const usable = machines.filter((machine) => machine.connected === true && !lacks(machine, "chat.serve"));
  const own = usable.map(({ id, name }) => ({ id, name, own: true, shared: shared.some((one) => one.id === id) }));
  const others = shared.filter((one) => one.connected === true && !machines.some(({ id }) => id === one.id));
  return [...own, ...others.map(({ id, name }) => ({ id, name, own: false, shared: true }))];
}

/** Where a person who may send (`sends`) stands on calling an agent, with their own `machines` and those `shared` with the workspace. */
export function readiness(sends: boolean, machines: MachineSummary[], shared: SharedMachine[] = []): Readiness {
  if (!sends) {
    return { kind: "unserved" };
  }
  const usable = callable(machines, shared);
  if (usable.length > 0) {
    return { kind: "ready", machines: usable };
  }
  if (machines.length === 0) {
    return { kind: "unpaired" };
  }
  return { kind: machines.some((machine) => machine.connected === true) ? "outdated" : "offline" };
}

const MISSING: Record<Exclude<Readiness["kind"], "ready">, string> = {
  unserved: "This server can't take messages yet.",
  unpaired: "Pair a machine in Settings to call your own agent.",
  offline: "Your machine is offline. Start Weacon on it to call your agent.",
  outdated: "Your machine's weacon is too old to run chats. Update it to call your agent.",
};

/** What is missing to call an agent, in one line. */
export function missingText(kind: Exclude<Readiness["kind"], "ready">): string {
  return MISSING[kind];
}

const NEW_CHAT: Subject = {
  subject: "the chat",
  refused: "couldn't be started",
  unsent: "wasn't started",
  unsure: "may have started",
  gone: "it can't be started",
};

/** Why a chat's first words that ended as `sent` didn't start it, or may not have; nothing when they did. */
export function startProblem(sent: Sent): string | undefined {
  if (sent === "too_long") {
    return sentProblem(sent);
  }
  return settledText(sent, NEW_CHAT);
}

/** What is known of the machine picked for a person's agent: its repositories, once its core answered, and whether an agent is signed in there for the workspace. */
export type Picked = { repos: RepoId[] | undefined; agent: boolean };

/** Why the person's agent can't be called on the machine picked yet, or `undefined` when it can. */
export function agentMissing({ repos, agent }: Picked): string | undefined {
  if (repos === undefined) {
    return "Looking for this machine's repositories.";
  }
  if (repos.length === 0) {
    return "Add a repository on this machine to call your agent.";
  }
  return agent ? undefined : "No agent is signed in for this workspace on this machine.";
}

/** Every agent a person can sign in. */
export const KINDS: AgentKind[] = ["claude", "codex"];

/** The agents a person can pick: every one until the machine answers, then those `signedIn` there for the workspace. */
export function offeredAgents(signedIn: AgentKind[] | undefined): AgentKind[] {
  return signedIn === undefined ? KINDS : KINDS.filter((kind) => signedIn.includes(kind));
}

/** What a machine is called where it is picked, with a mark on one shared with the workspace. */
export function machineLabel({ name, shared }: Callable): string {
  return shared ? `${name} (shared)` : name;
}

/** A person's own agent in a chat, as their first prompt there fixes it. */
export type Choice = { machine: MachineId; repo: RepoId; agent: AgentKind };
