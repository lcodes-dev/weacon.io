import { type Sent, sentProblem } from "../composer/prompt.ts";
import type { AgentKind, MachineId, MachineSettings, MachineSummary, RepoId } from "../protocol.ts";
import { lacks } from "../machines/core.ts";
import { type Subject, settledText } from "../receipts.ts";

/** Whether a person can call an agent of their own: on which of their machines, or what is missing first. */
export type Readiness = { kind: "ready"; machines: MachineSummary[] } | { kind: "unserved" | "unpaired" | "offline" | "outdated" };

/** Where a person who may send (`sends`) with `machines` stands on calling their own agent. */
export function readiness(sends: boolean, machines: MachineSummary[]): Readiness {
  if (!sends) {
    return { kind: "unserved" };
  }
  const usable = machines.filter((machine) => machine.connected === true && !lacks(machine, "chat.serve") && !lacks(machine, "chat.answers"));
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

const KINDS: AgentKind[] = ["claude", "codex"];

/** The agents a person can pick: every one until the machine answers, then those signed in there for the workspace. */
export function signedIn(settings: MachineSettings | undefined): AgentKind[] {
  return settings === undefined ? KINDS : KINDS.filter((kind) => settings.sign_ins.some(({ agent, state }) => agent === kind && state === "signed_in"));
}

/** A person's own agent in a chat, as their first prompt there fixes it. */
export type Choice = { machine: MachineId; repo: RepoId; agent: AgentKind };
