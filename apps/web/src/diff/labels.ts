import type { DiffContent, FileChange } from "../protocol.ts";
import type { Changes, DiffRow } from "./changes.ts";

/** What the page says above a chat's files, or instead of them. */
export function summary({ asked, files, total, cut }: Changes): string {
  if (total === 0) {
    return "No changes.";
  }
  if (total === undefined) {
    return cut || asked === undefined ? "The changes couldn't load." : "Loading changes.";
  }
  const changed = total === 1 ? "1 file changed" : `${total} files changed`;
  if (files.length >= total) {
    return changed;
  }
  return cut ? `${changed}, only ${files.length} loaded` : `${changed}, ${files.length} loaded`;
}

export function changeText(change: FileChange): string {
  switch (change.kind) {
    case "added":
      return "Added";
    case "removed":
      return "Removed";
    case "modified":
      return "Modified";
    case "renamed":
      return `Renamed from ${change.from}`;
  }
}

/** What a file shows instead of lines, if it has none to show. */
export function contentText(content: DiffContent): string | undefined {
  switch (content.kind) {
    case "text":
      return content.hunks.length === 0 ? "Only the file's mode changed." : undefined;
    case "binary":
      return "Binary file.";
    case "too_large":
      return "Too large to show.";
  }
}

/** Where a hunk starts; a side with no lines starts at 0 and isn't named. */
export function hunkText(old: number, next: number): string {
  const sides = [old > 0 && `old line ${old}`, next > 0 && `new line ${next}`].filter(Boolean);
  return `From ${sides.join(", ")}`;
}

/**
 * Which lines rows `first` to `last` are, as a phrase such as "lines 12 to 13": by their new number,
 * or their old one on removed lines; none when the rows aren't lines.
 */
export function linesPhrase(rows: readonly DiffRow[], first: number, last: number): string | undefined {
  const from = rows[first];
  const to = rows[last];
  if (from?.kind !== "line" || to?.kind !== "line") {
    return undefined;
  }
  if (first === last) {
    return `line ${ref(from)}`;
  }
  // Once a range starts on the old side, an end on the new side says so.
  const side = from.new === undefined && to.new !== undefined ? "new " : "";
  return `lines ${ref(from)} to ${side}${ref(to)}`;
}

/** `linesPhrase` as a label of its own, "Lines" when the rows aren't lines. */
export function linesText(rows: readonly DiffRow[], first: number, last: number): string {
  const phrase = linesPhrase(rows, first, last) ?? "lines";
  return `${phrase.charAt(0).toUpperCase()}${phrase.slice(1)}`;
}

function ref(row: DiffRow & { kind: "line" }): string {
  return row.new === undefined ? `old ${row.old}` : `${row.new}`;
}
