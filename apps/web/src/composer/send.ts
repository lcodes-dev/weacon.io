import { useEffect, useRef, useState } from "react";

import { newRequestId } from "../ids.ts";
import type { Lates, Send } from "./prompt.ts";
import { type Ended, Sends } from "./sends.ts";

/** One box's `Sends` for React: `taken` hears each text once it is accepted, at once or late. */
export function useSend(send: Send, lates: Lates, taken: (text: string) => void) {
  const [sending, setSending] = useState(false);
  const [problem, setProblem] = useState<string>();
  const sends = useRef(new Sends());
  const heard = useRef(taken);
  heard.current = taken;
  const show = (ended: Ended) => {
    if (ended.latest) {
      setProblem(ended.problem);
    }
    if (ended.taken) {
      heard.current(ended.text);
    }
  };
  useEffect(
    () =>
      lates((late) => {
        const ended = sends.current.late(late);
        if (ended !== undefined) {
          show(ended);
        }
      }),
    [lates],
  );
  const submit = async (text: string) => {
    const request = sends.current.start(text, newRequestId);
    setSending(true);
    setProblem(undefined);
    const sent = await send(text, request);
    setSending(false);
    show(sends.current.end(text, request, sent));
  };
  return { sending, problem, submit, clear: () => setProblem(undefined) };
}
