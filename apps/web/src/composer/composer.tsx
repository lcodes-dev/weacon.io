import { useRef, useState } from "react";

import "./composer.css";
import { sends } from "./keys.ts";
import { type Retry, type Send, after, requestFor, requestId } from "./prompt.ts";

/**
 * The message box. While `blocked` says why, it still takes text, but can't send it. It keeps the
 * text until the server accepts it, and says so when it doesn't.
 */
export function Composer({ send, blocked, hidden }: { send: Send | undefined; blocked: string | undefined; hidden: boolean }) {
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const [problem, setProblem] = useState<string>();
  const retry = useRef<Retry>(undefined);
  const hint = problem ?? blocked;
  const ready = !sending && blocked === undefined && send !== undefined && text.trim() !== "";
  const submit = async () => {
    if (!ready) {
      return;
    }
    const request = requestFor(retry.current, text, () => requestId(crypto.getRandomValues(new Uint8Array(16))));
    setSending(true);
    setProblem(undefined);
    const next = after(await send(text, request), text, request);
    setSending(false);
    setText(next.text);
    setProblem(next.problem);
    retry.current = next.retry;
  };
  return (
    <form
      className="composer"
      hidden={hidden}
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
    >
      <textarea
        aria-label="Message"
        rows={2}
        value={text}
        readOnly={sending}
        onChange={(event) => setText(event.target.value)}
        onKeyDown={(event) => {
          if (sends(event.nativeEvent)) {
            event.preventDefault();
            void submit();
          }
        }}
      />
      <button type="submit" disabled={!ready}>
        Send
      </button>
      {hint !== undefined && <p className="hint muted">{hint}</p>}
    </form>
  );
}
