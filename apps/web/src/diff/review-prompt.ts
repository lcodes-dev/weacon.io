import type { DiffFile } from "../protocol.ts";
import { rows } from "./changes.ts";
import type { Comment } from "./comments.ts";
import { linesText } from "./labels.ts";

/**
 * The prompt that sends `comments` on `files` to the chat's agent: each comment's file, side and
 * lines, then its words quoted. The agent reads the lines from the file itself.
 */
export function reviewPrompt(files: readonly DiffFile[], comments: readonly Comment[]): string {
  const byPath = new Map(files.map((file) => [file.path, file.content.kind === "text" ? rows(file.content.hunks) : []]));
  const quoted = comments.map(({ path, first, last, text }) => {
    const where = linesText(byPath.get(path) ?? [], first, last);
    const words = text.trim().split("\n").map((line) => `> ${line}`);
    return [`${path}, ${where.charAt(0).toLowerCase()}${where.slice(1)}:`, ...words].join("\n");
  });
  return ["Review comments on your changes:", ...quoted].join("\n\n");
}
