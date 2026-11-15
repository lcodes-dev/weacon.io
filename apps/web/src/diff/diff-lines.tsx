import { useMemo } from "react";

import { lineDiff } from "./line-diff.ts";
import "./diff-lines.css";

/** How `after` differs from `before`, line by line, with the lines added and removed marked. */
export function DiffLines({ before, after, label }: { before: string; after: string; label: string }) {
  const lines = useMemo(() => lineDiff(before, after), [before, after]);
  return (
    <div className="diff-lines" role="group" aria-label={label}>
      {lines.map(({ kind, text }, at) => {
        const line = text === "" ? " " : text;
        return kind === "added" ? <ins key={at}>{line}</ins> : kind === "removed" ? <del key={at}>{line}</del> : <span key={at}>{line}</span>;
      })}
    </div>
  );
}
