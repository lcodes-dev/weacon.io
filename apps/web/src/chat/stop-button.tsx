import "./chat.css";

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
