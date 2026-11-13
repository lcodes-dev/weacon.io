import { useState } from "react";

import "./composer.css";
import { sends } from "./keys.ts";

/** The message box. While `blocked` says why, it still takes text, but can't send it. */
export function Composer({
  send,
  blocked,
  hidden,
}: {
  send: ((text: string) => void) | undefined;
  blocked: string | undefined;
  hidden: boolean;
}) {
  const [text, setText] = useState("");
  const ready = blocked === undefined && send !== undefined && text.trim() !== "";
  const submit = () => {
    if (ready) {
      send(text);
      setText("");
    }
  };
  return (
    <form
      className="composer"
      hidden={hidden}
      onSubmit={(event) => {
        event.preventDefault();
        submit();
      }}
    >
      <textarea
        aria-label="Message"
        rows={2}
        value={text}
        onChange={(event) => setText(event.target.value)}
        onKeyDown={(event) => {
          if (sends(event.nativeEvent)) {
            event.preventDefault();
            submit();
          }
        }}
      />
      <button type="submit" disabled={!ready}>
        Send
      </button>
      {blocked !== undefined && <p className="hint muted">{blocked}</p>}
    </form>
  );
}
