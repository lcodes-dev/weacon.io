import { useEffect, useRef, useState } from "react";

import { newId } from "../ids.ts";
import type { Lates, Send } from "./prompt.ts";
import { type Ended, Sends } from "./sends.ts";

/** One box's `Sends` for React: `taken` hears each send's payload once it is accepted, at once or late. */
export function useSend<T>(send: Send, lates: Lates, taken: (payload: T) => void) {
  const [sending, setSending] = useState(false);
  const [problem, setProblem] = useState<string>();
  const sends = useRef(new Sends<T>());
  const heard = useRef(taken);
  heard.current = taken;
  const show = (ended: Ended<T> | undefined) => {
    if (ended?.latest === true) {
      setProblem(ended.problem);
    }
    if (ended?.taken === true) {
      heard.current(ended.payload);
    }
  };
  useEffect(() => lates((late) => show(sends.current.settle(late.request, late.settled))), [lates]);
  const submit = async (text: string, payload: T) => {
    const request = sends.current.start(text, payload, newId);
    setSending(true);
    setProblem(undefined);
    const sent = await send(text, request);
    setSending(false);
    show(sends.current.settle(request, sent));
  };
  return { sending, problem, submit, clear: () => setProblem(undefined) };
}
