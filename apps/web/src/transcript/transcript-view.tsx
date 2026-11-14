import { type RefObject, memo, useLayoutEffect, useRef } from "react";

import { type Answers, askKey } from "../chat/asks.ts";
import type { Line } from "../chat/lines.ts";
import { AskView } from "../chat/parts.tsx";
import type { Outcome, RunId } from "../protocol.ts";
import { MarkdownView } from "./markdown-view.ts";
import { useFollow, useHeldFormatting } from "./transcript-hooks.ts";
import type { Row, ToolState } from "./transcript.ts";

/** What answering a chat's permission prompts needs: the answers on their way, and a way to send one. */
export type Answerer = { answers: Answers; answer: (run: RunId, prompt: number, allow: boolean) => void };

/**
 * A chat's lines, following new ones down unless the reader has scrolled up or is selecting.
 * While `away`, under Settings, selections there neither hold it still nor hold rows back.
 */
export function TranscriptView({
  lines,
  away,
  answering,
  older,
  loading,
  retry,
}: {
  lines: readonly Line[];
  away: boolean;
  answering: Answerer;
  /** Shows the events before the first one shown, when there are any. */
  older: (() => void) | undefined;
  /** Whether the chat's first snapshot is on its way. */
  loading: boolean;
  /** Asks for the chat again, when the server refused it. */
  retry: (() => void) | undefined;
}) {
  const scroller = useRef<HTMLDivElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const { mayFormat, refreshes } = useHeldFormatting(scroller, away);
  const onScroll = useFollow(scroller, content, { rows: lines, refreshes, away });
  const kept = useKeptPlace(scroller, content, lines);

  const problem = retry !== undefined && (
    <p className="row problem" role="alert">
      The chat couldn't load.{" "}
      <button type="button" onClick={retry}>
        Try again
      </button>
    </p>
  );
  if (lines.length === 0 && older === undefined) {
    return problem || <p className="page muted">{loading ? "Loading" : "No messages yet."}</p>;
  }
  return (
    <div className="page transcript" ref={scroller} onScroll={onScroll}>
      <div ref={content}>
        {problem}
        {older !== undefined && (
          <button
            type="button"
            className="older"
            onClick={() => {
              kept();
              older();
            }}
          >
            Show earlier messages
          </button>
        )}
        {lines.map((line) => (
          <LineView
            key={line.key}
            line={line}
            formatted={line.kind === "row" && line.row.kind === "text" && !line.writing && mayFormat(line.key)}
            answering={answering}
          />
        ))}
      </div>
    </div>
  );
}

/**
 * Keeps the first row in view where it is once older lines come in above it, so loading earlier
 * messages doesn't move what the reader sees. Call the result just before asking for them.
 */
function useKeptPlace(
  scroller: RefObject<HTMLDivElement | null>,
  content: RefObject<HTMLDivElement | null>,
  lines: readonly Line[],
): () => void {
  const place = useRef<{ first: string | undefined; row: Element; top: number }>(undefined);
  // After `useFollow`'s own layout effect, so this has the last word on where the view is.
  useLayoutEffect(() => {
    const [element, kept] = [scroller.current, place.current];
    if (element === null || kept === undefined || lines[0]?.key === kept.first) {
      return;
    }
    place.current = undefined;
    element.scrollTop = kept.row.getBoundingClientRect().top - element.getBoundingClientRect().top + element.scrollTop - kept.top;
  }, [lines]);
  return () => {
    const [element, row] = [scroller.current, content.current?.querySelector(".row")];
    if (element !== null && row !== null && row !== undefined) {
      place.current = { first: lines[0]?.key, row, top: row.getBoundingClientRect().top - element.getBoundingClientRect().top };
    }
  };
}

function LineView({ line, formatted, answering }: { line: Line; formatted: boolean; answering: Answerer }) {
  switch (line.kind) {
    case "prompt":
      return <div className="row prompt">{line.text}</div>;
    case "agent":
      return <div className="row agent">{line.label}</div>;
    case "ask":
      return (
        <AskView
          ask={line.ask}
          answering={answering.answers.get(askKey(line.run, line.ask.prompt))}
          onAnswer={(allow) => answering.answer(line.run, line.ask.prompt, allow)}
        />
      );
    case "row":
      return <RowView rowKey={line.key} row={line.row} writing={line.writing} formatted={formatted} />;
  }
}

const RowView = memo(function RowView({
  rowKey,
  row,
  writing,
  formatted,
}: {
  rowKey: string;
  row: Row;
  writing: boolean;
  formatted: boolean;
}) {
  switch (row.kind) {
    case "text":
      return (
        <div className="row text" data-key={rowKey} data-writing={writing || undefined}>
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

// Row labels have no full stop; whole sentences, such as the empty transcript's, have one.
const ENDINGS: Record<Outcome["state"], string> = {
  completed: "Done",
  interrupted: "Stopped",
  failed: "Failed",
  accepted: "Ended",
  unknown: "Ended",
};

function finishedText(state: Outcome["state"]): string {
  return ENDINGS[state] ?? "Ended";
}
