import "./chat.css";

import { type Answering, answerProblem, askText } from "./asks.ts";
import type { Ask } from "./chat.ts";

/** A permission prompt: the tool the agent asks to use, and Allow and Deny while it waits, for whoever may answer. */
export function AskView({
  ask,
  answering,
  onAnswer,
}: {
  ask: Ask;
  answering: Answering | undefined;
  onAnswer: ((allow: boolean) => void) | undefined;
}) {
  const sending = answering !== undefined && answering.settled === undefined;
  const problem = answerProblem(answering);
  return (
    <div className={ask.state === "open" ? "row ask open" : "row ask"} role="group" aria-label={`Use ${ask.tool === "" ? "a tool" : ask.tool}?`}>
      <span>{askText(ask)}</span>
      {ask.state === "open" && onAnswer !== undefined && (
        <span className="choices">
          <button type="button" disabled={sending} onClick={() => onAnswer(true)}>
            Allow
          </button>
          <button type="button" disabled={sending} onClick={() => onAnswer(false)}>
            Deny
          </button>
        </span>
      )}
      {ask.state === "open" && problem !== undefined && (
        <span className="problem" role="alert">
          {problem}
        </span>
      )}
    </div>
  );
}
