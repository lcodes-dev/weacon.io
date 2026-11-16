import "./chat.css";

import { type Answering, answerProblem, askText, toolName } from "./asks.ts";
import type { Ask } from "./chat.ts";

/**
 * A permission prompt: the tool the agent asks to use, and Allow and Deny while it waits, for its
 * owner; anyone else reads that it `waitsFor` them.
 */
export function AskView({
  ask,
  answering,
  onAnswer,
  waitsFor,
}: {
  ask: Ask;
  answering: Answering | undefined;
  onAnswer: ((allow: boolean) => void) | undefined;
  waitsFor: string | undefined;
}) {
  const sending = answering !== undefined && answering.settled === undefined;
  const problem = answerProblem(answering);
  return (
    <div className={ask.state === "open" ? "row ask open" : "row ask"} role="group" aria-label={`Use ${toolName(ask.tool)}?`}>
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
      {ask.state === "open" && onAnswer === undefined && waitsFor !== undefined && <span className="waits">Waiting for {waitsFor}</span>}
      {ask.state === "open" && problem !== undefined && (
        <span className="problem" role="alert">
          {problem}
        </span>
      )}
    </div>
  );
}
