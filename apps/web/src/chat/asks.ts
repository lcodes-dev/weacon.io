import type { RequestId, RunId } from "../protocol.ts";
import type { Ask } from "./chat.ts";
import { type Settled, type Subject, settledText } from "../receipts.ts";

/** A person's answer to the prompt `prompt` of `run`, on its way or not taken. */
export type Answering = {
  run: RunId;
  prompt: number;
  allow: boolean;
  request: RequestId;
  settled: Exclude<Settled, "accepted"> | undefined;
};

/** Answers by prompt, keyed by `askKey`. */
export type Answers = ReadonlyMap<string, Answering>;

export function askKey(run: RunId, prompt: number): string {
  return `${run}#${prompt}`;
}

/** `answers` with the answer to the prompt `prompt` of `run` on its way under `request`. */
export function sending(answers: Answers, { run, prompt, allow }: { run: RunId; prompt: number; allow: boolean }, request: RequestId): Answers {
  return new Map(answers).set(askKey(run, prompt), { run, prompt, allow, request, settled: undefined });
}

/** `answers` once the answer sent under `request` for `key` ended as `settled`. */
export function settle(answers: Answers, key: string, request: RequestId, settled: Settled): Answers {
  const before = answers.get(key);
  if (before?.request !== request) {
    return answers;
  }
  const next = new Map(answers);
  if (settled === "accepted") {
    next.delete(key);
    return next;
  }
  return next.set(key, { ...before, settled });
}

const ANSWER: Subject = {
  subject: "the answer",
  refused: "wasn't taken",
  unsent: "wasn't sent",
  unsure: "may not have been sent",
  gone: "it takes no answers",
};
const STOP: Subject = {
  subject: "the agent",
  refused: "wasn't stopped",
  unsent: "wasn't stopped",
  unsure: "may not have stopped",
  gone: "there's no agent to stop",
};

/** What Stop says once its interrupt ended as `settled`: nothing when it was taken. */
export function stopProblem(settled: Settled): string | undefined {
  return settledText(settled, STOP);
}

/** What a prompt says about its answer: nothing, or why it may not have been taken. */
export function answerProblem(answering: Answering | undefined): string | undefined {
  return answering?.settled === undefined ? undefined : settledText(answering.settled, ANSWER);
}

/** What a permission prompt says; one stored before prompts kept their tools names none. */
export function askText({ state, tool }: Pick<Ask, "state" | "tool">): string {
  switch (state) {
    case "open":
      return `Asks to use ${toolName(tool)}`;
    case "closed":
      return `Asked to use ${toolName(tool)}`;
    case "allowed":
      return tool === "" ? "Allowed" : `Allowed ${tool}`;
    case "denied":
      return tool === "" ? "Denied" : `Denied ${tool}`;
  }
}

/** What a waiting prompt shows of what its tool wants, and whether more was left out; nothing once it no longer waits. */
export function wantsShown({ state, wants }: Pick<Ask, "state" | "wants">): { text: string; more: string | undefined } | undefined {
  if (state !== "open" || wants === undefined) {
    return undefined;
  }
  return { text: wants.text, more: wants.cut === true ? "The rest is too long to show." : undefined };
}

/** The tool a prompt names, or "a tool" when its name wasn't kept. */
export function toolName(tool: string): string {
  return tool === "" ? "a tool" : tool;
}
