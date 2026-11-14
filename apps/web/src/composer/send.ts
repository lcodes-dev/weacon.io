import { useEffect, useRef, useState } from "react";

import { newRequestId } from "../ids.ts";
import { type After, type Late, type Retry, type Send, after, afterLate, requestFor } from "./prompt.ts";

/**
 * Sends one text at a time through `send`, sending the same text again under the same request so
 * it can't run twice, and settles a send whose receipt comes `late`. `taken` hears each text once
 * it is accepted, at once or late.
 */
export function useSend(send: Send, late: Late | undefined, taken: (text: string) => void) {
  const [sending, setSending] = useState(false);
  const [problem, setProblem] = useState<string>();
  const retry = useRef<Retry>(undefined);
  const settle = (next: After, text: string) => {
    setProblem(next.problem);
    retry.current = next.retry;
    if (next.text === "") {
      taken(text);
    }
  };
  useEffect(() => {
    const sent = retry.current;
    const next = late === undefined || sending || sent === undefined ? undefined : afterLate(sent, late);
    if (sent !== undefined && next !== undefined) {
      settle(next, sent.text);
    }
  }, [late]);
  const submit = async (text: string) => {
    const request = requestFor(retry.current, text, newRequestId);
    setSending(true);
    setProblem(undefined);
    const next = after(await send(text, request), text, request);
    setSending(false);
    settle(next, text);
  };
  return { sending, problem, submit, clear: () => setProblem(undefined) };
}
