import "./chat.css";

import { type Answering, answerProblem } from "./asks.ts";
import type { Ask } from "./chat.ts";

/** A permission prompt: the tool the agent asks to use, and Allow and Deny while it waits. */
export function AskView({
  ask,
  answering,
  onAnswer,
}: {
  ask: Ask;
  answering: Answering | undefined;
  onAnswer: (allow: boolean) => void;
}) {
  const sending = answering !== undefined && answering.settled === undefined;
  const problem = answerProblem(answering);
  return (
    <div className={ask.state === "open" ? "row ask open" : "row ask"} role="group" aria-label={`Use ${ask.tool}?`}>
      <span>{askText(ask)}</span>
      {ask.state === "open" && (
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

function askText({ state, tool }: Ask): string {
  switch (state) {
    case "open":
      return `Asks to use ${tool}`;
    case "closed":
      return `Asked to use ${tool}`;
    case "allowed":
      return `Allowed ${tool}`;
    case "denied":
      return `Denied ${tool}`;
  }
}

/** Stop, shown while an agent is on a turn, and why the last one may not have worked. */
export function StopButton({ stopping, problem, onStop }: { stopping: boolean; problem: string | undefined; onStop: () => void }) {
  return (
    <>
      {problem !== undefined && (
        <span className="problem" role="alert">
          {problem}
        </span>
      )}
      <button type="button" className="stop" disabled={stopping} onClick={onStop}>
        {stopping ? "Stopping" : "Stop"}
      </button>
    </>
  );
}
