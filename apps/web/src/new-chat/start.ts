import type { ChatRef } from "../chat-ref.ts";
import { type Sent, sentProblem } from "../composer/prompt.ts";
import type { AgentKind, ChatAction, ChatId, MachineId, MachineSettings, MachineSummary, RepoId, RequestId, SignInState, WorkspaceId } from "../protocol.ts";
import { lacks } from "../machines/core.ts";
import { type Subject, mayHaveRun, settledText } from "../receipts.ts";

/** Whether a person can start a chat: on which of their machines, or what is missing first. */
export type Readiness = { kind: "ready"; machines: MachineSummary[] } | { kind: "unserved" | "unpaired" | "offline" | "outdated" };

/** Where a person who may send (`sends`) with `machines` stands on starting a chat. */
export function readiness(sends: boolean, machines: MachineSummary[]): Readiness {
  if (!sends) {
    return { kind: "unserved" };
  }
  const usable = machines.filter((machine) => machine.connected === true && !lacks(machine, "chat.serve"));
  if (usable.length > 0) {
    return { kind: "ready", machines: usable };
  }
  if (machines.length === 0) {
    return { kind: "unpaired" };
  }
  return { kind: machines.some((machine) => machine.connected === true) ? "outdated" : "offline" };
}

const MISSING: Record<Exclude<Readiness["kind"], "ready">, string> = {
  unserved: "This server can't start chats yet.",
  unpaired: "Pair a machine in Settings to start a chat.",
  offline: "Your machine is offline. Start Weacon on it to start a chat.",
  outdated: "Your machine's weacon is too old to run chats. Update it to start a chat.",
};

/** What is missing to start a chat, in one line. */
export function missingText(kind: Exclude<Readiness["kind"], "ready">): string {
  return MISSING[kind];
}

/** What a new chat's first prompt names besides its text. */
export type Choice = { machine: MachineId; repo: RepoId; agent: AgentKind };

/** A first prompt on its way: the chat it makes and what it asks. */
export type Start = { chatRef: ChatRef; action: Extract<ChatAction, { kind: "prompt" }> };

/**
 * The first prompts of one new-chat box, by request: a retry of one that may have run goes
 * again to the same chat with the same choice, so it can't make two.
 */
export class Starts {
  #held = new Map<RequestId, Start>();

  /** What to send under `request`: what it sent before, or `text` and `choice` to a chat `fresh` names. */
  begin(request: RequestId, workspace: WorkspaceId, text: string, choice: Choice, fresh: () => ChatId): Start {
    const held = this.#held.get(request);
    if (held !== undefined) {
      return held;
    }
    const start: Start = { chatRef: { workspace, chat: fresh() }, action: { kind: "prompt", text, ...choice } };
    this.#held.set(request, start);
    return start;
  }

  /** The chat the first prompt under `request` made, once it ended as `sent`, if it did; only the first word counts. */
  end(request: RequestId, sent: Sent): ChatId | undefined {
    const held = this.#held.get(request);
    if (held === undefined || (sent !== "too_long" && mayHaveRun(sent))) {
      return undefined;
    }
    this.#held.delete(request);
    return sent === "accepted" ? held.chatRef.chat : undefined;
  }
}

const NEW_CHAT: Subject = {
  subject: "the chat",
  refused: "couldn't be started",
  unsent: "wasn't started",
  unsure: "may have started",
  gone: "it can't be started",
};

/** Why a first prompt that ended as `sent` didn't start its chat, or may not have; nothing when it did. */
export function startProblem(sent: Sent): string | undefined {
  if (sent === "too_long") {
    return sentProblem(sent);
  }
  return settledText(sent, NEW_CHAT);
}

/** What the new-chat box knows of the machine picked: its repositories and the agent's sign-in, once its core answered. */
export type Picked = { live: boolean; repos: RepoId[] | undefined; agent: string; signIn: SignInState | undefined };

/** Why the box can't start a chat on the machine picked now, or `undefined` when it can. */
export function startBlocked({ live, repos, agent, signIn }: Picked): string | undefined {
  if (!live) {
    return "Chats start once the server is connected.";
  }
  if (repos === undefined) {
    return "Pick a repository to start a chat.";
  }
  if (repos.length === 0) {
    return "Add a repository on this machine to start a chat.";
  }
  if (signIn === "signed_out") {
    return `${agent} isn't signed in on this machine.`;
  }
  return signIn === "not_installed" ? `${agent} isn't installed on this machine.` : undefined;
}

const KINDS: AgentKind[] = ["claude", "codex"];

/** The agents New chat offers: every one until the machine answers, then those signed in there for the workspace. */
export function signedIn(settings: MachineSettings | undefined): AgentKind[] {
  return settings === undefined ? KINDS : KINDS.filter((kind) => settings.sign_ins.some(({ agent, state }) => agent === kind && state === "signed_in"));
}
