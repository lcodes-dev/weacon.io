import type { ChatRef } from "../chat-ref.ts";
import type { Incoming } from "../incoming.ts";
import type { Envelope, MachineId, RequestId, WorktreeAction } from "../protocol.ts";

/** A worktree's state, as its core last said. */
export type State = { kind: "state"; removed: boolean; uncommitted: boolean; unpushed: boolean };

/** Where a chat's worktree stands: not known yet, none yet, or its state. */
export type Standing = { kind: "unknown" } | { kind: "none" } | State;

/** What the review bar asks of the core: an action, or the state, which Remove asks first to `confirm` against. */
export type Ask = WorktreeAction | { kind: "state"; confirm?: boolean };

/** What came of the last action: a page to open, a note, or why it didn't work. */
export type Outcome = { kind: "link"; text: string; url: string } | { kind: "note"; text: string } | { kind: "problem"; text: string };

/** An ask on its way: its envelope id, which grows on a socket, so a later ask has a greater one. */
export type Pending = { id: number; request: RequestId; ask: Ask["kind"]; confirm: boolean };

/** A chat's worktree for its review bar: what is known, what is asked, and what came of it. */
export type Worktree = {
  standing: Standing;
  pending: readonly Pending[];
  // The id of the ask whose answer is shown, so an older answer can't replace a newer one.
  shown: number;
  outcome: Outcome | undefined;
  /** The fresh state Remove is being confirmed against. */
  confirming: State | undefined;
};

export const UNASKED: Worktree = { standing: { kind: "unknown" }, pending: [], shown: 0, outcome: undefined, confirming: undefined };

/** The envelope that asks `ask` of the worktree at `ref`, the chat's on a machine, as message `id` under `request`. */
export function envelopeFor(ref: ChatRef & { machine: MachineId }, id: number, request: RequestId, ask: Ask): Envelope {
  if (ask.kind === "state") {
    return { id, type: "worktree_query", ...ref, request, query: { kind: "state" } };
  }
  return { id, type: "worktree_command", ...ref, request, action: ask };
}

/** `worktree` waiting for the answer to `pending`; an action, Remove's state included, clears the last one's outcome. */
export function waiting(worktree: Worktree, pending: Pending): Worktree {
  const outcome = pending.ask === "state" && !pending.confirm ? worktree.outcome : undefined;
  return { ...worktree, pending: [...worktree.pending, pending], outcome };
}

/** `worktree` once `message` answers one of its asks about the chat at `ref`; anything else leaves it as it is. */
export function answered(worktree: Worktree, ref: ChatRef, message: Extract<Incoming, { type: "worktree_answer" | "error" }>): Worktree {
  if (message.type === "worktree_answer" && (message.workspace !== ref.workspace || message.chat !== ref.chat)) {
    return worktree;
  }
  const asked = worktree.pending.find((pending) => (message.type === "error" ? pending.id === message.id : pending.request === message.request));
  if (asked === undefined) {
    return worktree;
  }
  const rest = { ...worktree, pending: worktree.pending.filter((pending) => pending !== asked) };
  if (message.type === "error") {
    if (message.code === "worktree.none") {
      return stood(rest, asked, { kind: "none" });
    }
    const text = message.code === "worktree.busy" ? "Stop the agent first, then remove the worktree." : PROBLEMS[asked.ask];
    return { ...rest, outcome: { kind: "problem", text } };
  }
  const { answer } = message;
  switch (answer.kind) {
    case "state":
      return stood(rest, asked, { kind: "state", removed: answer.removed, uncommitted: answer.uncommitted, unpushed: answer.unpushed });
    case "pull_request_opened":
      return { ...rest, outcome: { kind: "link", text: "A draft pull request is open.", url: answer.url } };
    case "pull_request_page":
      return { ...rest, outcome: { kind: "link", text: "The branch is pushed. Open the pull request from its page.", url: answer.url } };
    case "terminal_opened":
      return { ...rest, outcome: { kind: "note", text: "A terminal opened on the chat's machine." } };
    default:
      return rest;
  }
}

// `worktree` standing as `asked`'s answer says, unless a later ask's answer is shown. Any newer
// standing ends a Remove being confirmed, except the fresh state Remove asked for, which starts it.
function stood(worktree: Worktree, asked: Pending, standing: Standing): Worktree {
  if (asked.id < worktree.shown) {
    return worktree;
  }
  const confirming = asked.confirm && standing.kind === "state" && !standing.removed ? standing : undefined;
  return { ...worktree, standing, shown: asked.id, confirming };
}

/** `worktree` with nothing waiting and nothing being confirmed, as when the connection ends; what it knows stays. */
export function ended(worktree: Worktree): Worktree {
  return { ...worktree, pending: [], confirming: undefined };
}

/** `worktree` without the Remove being confirmed. */
export function cancelled(worktree: Worktree): Worktree {
  return { ...worktree, confirming: undefined };
}

/** `worktree` saying `ask` couldn't be sent, as while the connection is down. */
export function unsent(worktree: Worktree, ask: Ask["kind"]): Worktree {
  return { ...worktree, outcome: { kind: "problem", text: `${PROBLEMS[ask]} The connection is down.` } };
}

/** Whether an action is on its way, so another waits. */
export function busy(worktree: Worktree): boolean {
  return worktree.pending.some((pending) => pending.ask !== "state" || pending.confirm);
}

const PROBLEMS: Record<Ask["kind"], string> = {
  state: "The worktree's state didn't come. Try again.",
  remove: "The worktree wasn't removed.",
  open_pull_request: "The pull request wasn't opened.",
  open_terminal: "The terminal didn't open. On a machine without a desktop terminal, run the line below there.",
};

/** The command that carries the chat on by hand in a terminal on its machine. */
export function resumeLine(chatRef: { workspace: string; chat: string }): string {
  return `weacon resume --workspace=${chatRef.workspace} --chat=${chatRef.chat}`;
}

/** One line on where the worktree stands. */
export function stateLine(standing: Standing): string {
  switch (standing.kind) {
    case "unknown":
      return "Checking the worktree.";
    case "none":
      return "This chat has no worktree yet.";
    case "state": {
      if (standing.removed) {
        return "The worktree was removed. The chat's next message checks it out again.";
      }
      const parts = [standing.uncommitted && "Uncommitted changes", standing.unpushed && "Commits not pushed"].filter((part) => part !== false);
      return parts.length === 0 ? "Everything is committed and pushed." : parts.join(" · ");
    }
  }
}

/** What Remove asks the person, given the worktree's fresh `state`: what goes, what stays, and how it comes back. */
export function removeQuestion(state: State): string {
  return [
    "Remove this chat's worktree?",
    state.uncommitted && "Its uncommitted changes are deleted for good.",
    state.unpushed ? "Its branch stays, with commits that aren't pushed yet." : "Its branch stays.",
    "The chat's next message checks it out again.",
  ]
    .filter((part) => part !== false)
    .join(" ");
}
