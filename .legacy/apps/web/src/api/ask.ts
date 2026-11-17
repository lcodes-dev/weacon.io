import { useState } from "react";

import type { Code } from "../protocol.ts";
import type { Answer } from "./api.ts";
import { problemText } from "./problems.ts";

/**
 * Asks the server one thing at a time: `busy` while it answers, then `then` its answer, or `problem`
 * why not, which `failed` is also told.
 */
export function useAsk() {
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string>();
  const ask = async <T>(request: () => Promise<Answer<T>>, then: (value: T) => void, failed?: (code: Code) => void) => {
    setBusy(true);
    setProblem(undefined);
    const answer = await request();
    setBusy(false);
    if (answer.ok) {
      then(answer.value);
    } else {
      setProblem(problemText(answer.code));
      failed?.(answer.code);
    }
  };
  return { busy, problem, ask, clear: () => setProblem(undefined) };
}
