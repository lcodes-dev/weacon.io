import { useState } from "react";

import { newId } from "../ids.ts";
import { type Send, type Sent, sentProblem } from "./prompt.ts";

/**
 * One box's sends for React, each under a fresh request: `taken` hears a send's payload once it is
 * accepted, and `explain` words why one wasn't, or may not have been.
 */
export function useSend<T>(send: Send, taken: (payload: T) => void, explain: (sent: Sent) => string | undefined = sentProblem) {
  const [sending, setSending] = useState(false);
  const [problem, setProblem] = useState<string>();
  const submit = async (text: string, payload: T) => {
    setSending(true);
    setProblem(undefined);
    const sent = await send(text, newId());
    setSending(false);
    setProblem(explain(sent));
    if (sent === "accepted") {
      taken(payload);
    }
  };
  return { sending, problem, submit, clear: () => setProblem(undefined) };
}
