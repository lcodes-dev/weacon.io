import { memo, useRef } from "react";

import { MarkdownView } from "./markdown-view.ts";
import type { Outcome } from "./protocol.ts";
import { useFollow, useHeldFormatting } from "./transcript-hooks.ts";
import type { Row, ToolState } from "./transcript.ts";

/** A run's rows, following new ones down unless the reader has scrolled up or is selecting. */
export function TranscriptView({ rows, writing, away }: { rows: readonly Row[]; writing: boolean; away: boolean }) {
  const scroller = useRef<HTMLDivElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const { mayFormat, refreshes } = useHeldFormatting(scroller, away);
  const onScroll = useFollow(scroller, content, { rows, refreshes, away });

  if (rows.length === 0) {
    return <p className="page muted">No run yet.</p>;
  }
  return (
    <div className="page transcript" ref={scroller} onScroll={onScroll}>
      <div ref={content}>
        {rows.map((row, at) => {
          const writingRow = writing && at === rows.length - 1;
          return (
            <RowView key={at} at={at} row={row} writing={writingRow} formatted={row.kind === "text" && !writingRow && mayFormat(at)} />
          );
        })}
      </div>
    </div>
  );
}

const RowView = memo(function RowView({
  at,
  row,
  writing,
  formatted,
}: {
  at: number;
  row: Row;
  writing: boolean;
  formatted: boolean;
}) {
  switch (row.kind) {
    case "text":
      return (
        <div className="row text" data-index={at} data-writing={writing || undefined}>
          {formatted ? <MarkdownView source={row.pieces.join("")} /> : row.pieces}
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
