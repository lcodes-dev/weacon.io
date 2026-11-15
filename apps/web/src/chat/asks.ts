import type { RequestId, RunId } from "../protocol.ts";
import type { Ask } from "./chat.ts";
import { type Settled, type Subject, mayHaveRun, settledText } from "../receipts.ts";

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

/**
 * `answers` with the answer to the prompt `prompt` of `run` on its way, and the request it goes
 * under. The same answer again, after one that may have been taken, goes under the same request,
 * so it can't run twice.
 */
export function sending(
  answers: Answers,
  { run, prompt, allow }: { run: RunId; prompt: number; allow: boolean },
  fresh: () => RequestId,
): { answers: Answers; request: RequestId } {
  const key = askKey(run, prompt);
  const before = answers.get(key);
  const again = before !== undefined && before.allow === allow && (before.settled === undefined || mayHaveRun(before.settled));
  const request = again ? before.request : fresh();
  return { answers: new Map(answers).set(key, { run, prompt, allow, request, settled: undefined }), request };
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

/** Every answer sent, by request, with the prompt it answers. */
export type SentAnswers = ReadonlyMap<RequestId, { run: RunId; prompt: number }>;

/**
 * `answers` once the answer sent under `request` ended as `settled`, on time or late; a request
 * that sent no answer changes nothing, and one a later answer replaced changes only its prompt.
 */
export function settleAnswer(answers: Answers, sent: SentAnswers, request: RequestId, settled: Settled): Answers {
  const answer = sent.get(request);
  return answer === undefined ? answers : settle(answers, askKey(answer.run, answer.prompt), request, settled);
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

/** What a permission prompt says; a stored chat keeps no tool names, so one may be missing. */
export function askText({ state, tool }: Pick<Ask, "state" | "tool">): string {
  const named = tool === "" ? undefined : tool;
  switch (state) {
    case "open":
      return `Asks to use ${named ?? "a tool"}`;
    case "closed":
      return `Asked to use ${named ?? "a tool"}`;
    case "allowed":
      return named === undefined ? "Allowed" : `Allowed ${named}`;
    case "denied":
      return named === undefined ? "Denied" : `Denied ${named}`;
  }
}
