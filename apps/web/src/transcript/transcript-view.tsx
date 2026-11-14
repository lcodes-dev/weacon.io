import { memo, useRef } from "react";

import type { Answers } from "../chat/asks.ts";
import { askKey } from "../chat/asks.ts";
import type { Line } from "../chat/lines.ts";
import { AskView } from "../chat/parts.tsx";
import type { Outcome } from "../protocol.ts";
import { MarkdownView } from "./markdown-view.ts";
import { useFollow, useHeldFormatting } from "./transcript-hooks.ts";
import type { Row, ToolState } from "./transcript.ts";

/** What answering a chat's permission prompts needs: the answers on their way, and a way to send one. */
export type Answering = { answers: Answers; answer: (run: string, prompt: number, allow: boolean) => void };

/**
 * A chat's lines, following new ones down unless the reader has scrolled up or is selecting.
 * While `away`, under Settings, selections there neither hold it still nor hold rows back.
 */
export function TranscriptView({
  lines,
  away,
  answering,
  older,
}: {
  lines: readonly Line[];
  away: boolean;
  answering: Answering;
  /** Shows the events before the first one shown, when there are any. */
  older: (() => void) | undefined;
}) {
  const scroller = useRef<HTMLDivElement>(null);
  const content = useRef<HTMLDivElement>(null);
  const { mayFormat, refreshes } = useHeldFormatting(scroller, away);
  const onScroll = useFollow(scroller, content, { rows: lines, refreshes, away });

  if (lines.length === 0 && older === undefined) {
    return <p className="page muted">No messages yet.</p>;
  }
  return (
    <div className="page transcript" ref={scroller} onScroll={onScroll}>
      <div ref={content}>
        {older !== undefined && (
          <button type="button" className="older" onClick={older}>
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

function LineView({ line, formatted, answering }: { line: Line; formatted: boolean; answering: Answering }) {
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
      return <RowView at={line.key} row={line.row} writing={line.writing} formatted={formatted} />;
  }
}

const RowView = memo(function RowView({
  at,
  row,
  writing,
  formatted,
}: {
  at: string;
  row: Row;
  writing: boolean;
  formatted: boolean;
}) {
  switch (row.kind) {
    case "text":
      return (
        <div className="row text" data-key={at} data-writing={writing || undefined}>
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
