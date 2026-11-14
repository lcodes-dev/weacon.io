import { requestFor } from "../composer/prompt.ts";
import type { RequestId, RunId } from "../protocol.ts";
import type { Settled } from "../receipts.ts";

/** A person's answer to a permission prompt, on its way or not taken. */
export type Answering = { allow: boolean; request: RequestId; settled: Exclude<Settled, "accepted"> | undefined };

/** Answers by prompt, keyed by `askKey`. */
export type Answers = ReadonlyMap<string, Answering>;

export function askKey(run: RunId, prompt: number): string {
  return `${run}#${prompt}`;
}

/**
 * `answers` with the answer to `key` on its way. The same answer again, after one that may have
 * been taken, goes under the same request, so it can't be taken twice.
 */
export function sending(answers: Answers, key: string, allow: boolean, fresh: () => RequestId): Answers {
  const before = answers.get(key);
  const retry = before === undefined || before.settled === "refused" ? undefined : { text: String(before.allow), request: before.request };
  const request = requestFor(retry, String(allow), fresh);
  return new Map(answers).set(key, { allow, request, settled: undefined });
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

const PROBLEMS: Record<Exclude<Settled, "accepted">, string> = {
  refused: "The answer wasn't taken",
  lost: "The connection closed; the answer may not have been sent",
  unsure: "The server didn't answer; the answer may not have been sent",
};

/** What a prompt says about its answer: nothing, or why it may not have been taken. */
export function answerProblem(answering: Answering | undefined): string | undefined {
  return answering?.settled === undefined ? undefined : PROBLEMS[answering.settled];
}
