import type { DiffContent, FileChange } from "../protocol.ts";
import type { Changes } from "./changes.ts";

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
    default:
      return "Changed";
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
    default:
      return "This app can't show this file.";
  }
}

/** Where a hunk starts; a side with no lines starts at 0 and isn't named. */
export function hunkText(old: number, next: number): string {
  const sides = [old > 0 && `old line ${old}`, next > 0 && `new line ${next}`].filter(Boolean);
  return `From ${sides.join(", ")}`;
}
