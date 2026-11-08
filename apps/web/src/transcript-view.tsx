import { memo, useLayoutEffect, useRef } from "react";

import { atBottom, selectingIn } from "./follow.ts";
import type { Outcome } from "./protocol.ts";
import type { Row, ToolState } from "./transcript.ts";

/** A run's rows, following new ones down unless the reader has scrolled up or is selecting. */
export function TranscriptView({ rows, writing }: { rows: readonly Row[]; writing: boolean }) {
  const scroller = useRef<HTMLDivElement>(null);
  const bottom = useRef(true);

  useLayoutEffect(() => {
    const element = scroller.current;
    if (element === null || !bottom.current) {
      return;
    }
    if (selectingIn(document.getSelection(), element)) {
      // Once the selection goes, follow only if the reader is still at the bottom.
      bottom.current = atBottom(element);
    } else {
      element.scrollTop = element.scrollHeight;
    }
  }, [rows]);

  if (rows.length === 0) {
    return <p className="page muted">No run yet.</p>;
  }
  return (
    <div
      className="page transcript"
      ref={scroller}
      onScroll={(event) => {
        bottom.current = atBottom(event.currentTarget);
      }}
    >
      {rows.map((row, at) => (
        <RowView key={at} row={row} writing={writing && at === rows.length - 1} />
      ))}
    </div>
  );
}

const RowView = memo(function RowView({ row, writing }: { row: Row; writing: boolean }) {
  switch (row.kind) {
    case "text":
      return (
        <div className="row text" data-writing={writing || undefined}>
          {row.pieces}
        </div>
      );
    case "tool":
      return <div className={`row tool ${row.state}`}>{toolText(row.tool, row.state)}</div>;
    case "finished":
      return <div className="row finished">{finishedText(row.outcome.state)}</div>;
  }
});

function toolText(tool: string, state: ToolState): string {
  return { running: `Running ${tool}`, done: `Ran ${tool}`, failed: `${tool} failed` }[state];
}

function finishedText(state: Outcome["state"]): string {
  return state === "completed" ? "Done" : `Turn ended: ${state}`;
}
