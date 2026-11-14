import type { Answering } from "./asks.ts";
import { answerProblem } from "./asks.ts";
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
    <div className={ask.open ? "row ask open" : "row ask"} role="group" aria-label={`Use ${ask.tool}?`}>
      <span>{ask.open ? `Asks to use ${ask.tool}` : `Asked to use ${ask.tool}`}</span>
      {ask.open && (
        <span className="choices">
          <button type="button" disabled={sending} onClick={() => onAnswer(true)}>
            Allow
          </button>
          <button type="button" disabled={sending} onClick={() => onAnswer(false)}>
            Deny
          </button>
        </span>
      )}
      {ask.open && problem !== undefined && (
        <span className="problem" role="alert">
          {problem}
        </span>
      )}
    </div>
  );
}

/** Stop, shown while an agent is on a turn. */
export function StopButton({ stopping, onStop }: { stopping: boolean; onStop: () => void }) {
  return (
    <button type="button" className="stop" disabled={stopping} onClick={onStop}>
      {stopping ? "Stopping" : "Stop"}
    </button>
  );
}
