import { memo, useEffect, useLayoutEffect, useReducer, useRef } from "react";

import { atBottom, selectingIn, touches } from "./follow.ts";
import { MarkdownView } from "./markdown-view.ts";
import type { Outcome } from "./protocol.ts";
import type { Row, ToolState } from "./transcript.ts";

/**
 * A run's rows, following new ones down unless the reader has scrolled up or is selecting. While
 * `away`, under Settings, selections there neither hold it still nor hold rows back.
 */
export function TranscriptView({ rows, writing, away }: { rows: readonly Row[]; writing: boolean; away: boolean }) {
  const scroller = useRef<HTMLDivElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const bottom = useRef(true);
  // Where the view last scrolled itself. Its scroll event comes a frame later, maybe after the
  // pane has shrunk, as when the message box comes back, so it says nothing about the reader.
  const followedTo = useRef<number | undefined>(undefined);
  // Text rows shown as Markdown; once formatted, a row stays so, as reformatting would undo a
  // selection in it. A finished row the selection touches waits until the selection leaves; a
  // bare caret, as a click leaves, holds it only while a button is down, as a drag may follow.
  const formatted = useRef(new Set<number>());
  const held = useRef(false);
  const pressed = useRef(false);
  const [refreshes, refresh] = useReducer((count: number) => count + 1, 0);

  useEffect(() => {
    const update = () => {
      if (held.current) {
        refresh();
      }
    };
    const press = () => {
      pressed.current = true;
    };
    const release = () => {
      pressed.current = false;
      update();
    };
    document.addEventListener("selectionchange", update);
    document.addEventListener("pointerdown", press);
    document.addEventListener("pointerup", release);
    return () => {
      document.removeEventListener("selectionchange", update);
      document.removeEventListener("pointerdown", press);
      document.removeEventListener("pointerup", release);
    };
  }, []);

  // Selections in Settings, while `away`, don't count.
  const selection = away ? null : document.getSelection();
  const awayNow = useRef(away);
  useLayoutEffect(() => {
    awayNow.current = away;
  }, [away]);
  const keepAtBottom = useRef(() => {
    const element = scroller.current;
    if (element === null || !bottom.current) {
      return;
    }
    if (selectingIn(awayNow.current ? null : document.getSelection(), element)) {
      // Once the selection goes, follow only if the reader is still at the bottom.
      bottom.current = atBottom(element);
    } else {
      element.scrollTop = element.scrollHeight;
      followedTo.current = element.scrollTop;
    }
  });
  useLayoutEffect(() => keepAtBottom.current(), [rows, refreshes]);

  const empty = rows.length === 0;
  useEffect(() => {
    const [element, inner] = [scroller.current, content.current];
    if (empty || element === null || inner === null) {
      return;
    }
    // Rows reach their real height only once shown, and the message box takes its room back when
    // Settings closes; a following reader stays at the bottom through both.
    const watch = new ResizeObserver(() => keepAtBottom.current());
    watch.observe(element);
    watch.observe(inner);
    return () => watch.disconnect();
  }, [empty]);

  if (empty) {
    return <p className="page muted">No run yet.</p>;
  }
  held.current = false;
  const format = (at: number): boolean => {
    if (formatted.current.has(at)) {
      return true;
    }
    const element = scroller.current?.querySelector(`[data-index="${at}"]`);
    if (element && (selectingIn(selection, element) || (pressed.current && touches(selection, element)))) {
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
        const element = event.currentTarget;
        if (element.scrollTop !== followedTo.current) {
          followedTo.current = undefined;
          bottom.current = atBottom(element);
        }
      }}
    >
      <div ref={content}>
        {rows.map((row, at) => {
          const writingRow = writing && at === rows.length - 1;
          return (
            <RowView key={at} at={at} row={row} writing={writingRow} formatted={row.kind === "text" && !writingRow && format(at)} />
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
