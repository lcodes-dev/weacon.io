import { memo } from "react";

import "./diff.css";

import type { DiffFile } from "../protocol.ts";
import { type Changes, type Row, rows } from "./changes.ts";
import { changeText, contentText, hunkText, summary } from "./labels.ts";

// Keep in step with `.diff .lines` in diff.css; an off-screen file reserves this much per row.
const ROW_PX = 20;

/** A chat's changed files, one section each, as their answers arrive. */
export function DiffView({ changes }: { changes: Changes }) {
  if (changes.files.length === 0) {
    return <p className="page muted">{summary(changes)}</p>;
  }
  return (
    <div className="page diff">
      <p className="muted">{summary(changes)}</p>
      {changes.files.map((file, at) => (
        <FileView key={at} file={file} />
      ))}
    </div>
  );
}

const FileView = memo(function FileView({ file }: { file: DiffFile }) {
  const { content } = file;
  const note = contentText(content);
  const lines = content.kind === "text" ? rows(content.hunks) : [];
  return (
    <section className="file" style={{ containIntrinsicSize: `auto ${(lines.length + 2) * ROW_PX}px` }}>
      <h2>
        <span className="path">{file.path}</span>
        <span className="muted">{changeText(file.change)}</span>
      </h2>
      {note === undefined ? (
        <div className="lines">
          {lines.map((row, at) => (
            <RowView key={at} row={row} />
          ))}
        </div>
      ) : (
        <p className="muted">{note}</p>
      )}
    </section>
  );
});

function RowView({ row }: { row: Row }) {
  if (row.kind === "hunk") {
    return <div className="note">{hunkText(row.old, row.new)}</div>;
  }
  return (
    <>
      <div className="line" data-op={row.op} data-old={row.old} data-new={row.new}>
        <span>{row.text}</span>
      </div>
      {row.noNewline && <div className="note">No newline at the end of the file.</div>}
    </>
  );
}
