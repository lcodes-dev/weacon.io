import { memo, useEffect, useLayoutEffect, useReducer, useRef } from "react";

import { atBottom, selectingIn, touches } from "./follow.ts";
import { MarkdownView } from "./markdown-view.tsx";
import type { Outcome } from "./protocol.ts";
import type { Row, ToolState } from "./transcript.ts";

/**
 * A run's rows, following new ones down unless the reader has scrolled up or is selecting. While
 * `away`, under Settings, selections there neither hold it still nor hold rows back.
 */
export function TranscriptView({ rows, writing, away }: { rows: readonly Row[]; writing: boolean; away: boolean }) {
  const scroller = useRef<HTMLDivElement>(null);
  const bottom = useRef(true);
  // Text rows shown as Markdown; once formatted, a row stays so, as reformatting would undo a
  // selection in it. A finished row the selection touches waits until the selection leaves.
  const formatted = useRef(new Set<number>());
  const held = useRef(false);
  const [, refresh] = useReducer((count: number) => count + 1, 0);

  useEffect(() => {
    const update = () => {
      if (held.current) {
        refresh();
      }
    };
    document.addEventListener("selectionchange", update);
    return () => document.removeEventListener("selectionchange", update);
  }, []);

  // Selections in Settings, while `away`, don't count.
  const selection = away ? null : document.getSelection();
  useLayoutEffect(() => {
    const element = scroller.current;
    if (element === null || !bottom.current) {
      return;
    }
    if (selectingIn(selection, element)) {
      // Once the selection goes, follow only if the reader is still at the bottom.
      bottom.current = atBottom(element);
    } else {
      element.scrollTop = element.scrollHeight;
    }
  }, [rows]);

  if (rows.length === 0) {
    return <p className="page muted">No run yet.</p>;
  }
  held.current = false;
  const format = (at: number): boolean => {
    if (formatted.current.has(at)) {
      return true;
    }
    const element = scroller.current?.querySelector(`[data-index="${at}"]`);
    if (element && touches(selection, element)) {
      held.current = true;
      return false;
    }
    formatted.current.add(at);
    return true;
  };
  return (
    <div
      className="page transcript"
      ref={scroller}
      onScroll={(event) => {
        bottom.current = atBottom(event.currentTarget);
      }}
    >
      {rows.map((row, at) => {
        const writingRow = writing && at === rows.length - 1;
        return (
          <RowView key={at} at={at} row={row} writing={writingRow} formatted={row.kind === "text" && !writingRow && format(at)} />
        );
      })}
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
