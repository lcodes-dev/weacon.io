import type { DiffFile } from "../protocol.ts";
import { rows } from "./changes.ts";
import type { Comment } from "./comments.ts";
import { linesText } from "./labels.ts";

/**
 * The prompt that sends `comments` on `files` to the chat's agent: each comment's file, side and
 * lines, then its words quoted. The agent reads the lines from the file itself.
 */
export function reviewPrompt(files: readonly DiffFile[], comments: readonly Comment[]): string {
  const quoted = comments.map(({ path, first, last, text }) => {
    const file = files.find((each) => each.path === path);
    const where = linesText(file?.content.kind === "text" ? rows(file.content.hunks) : [], first, last);
    // A file name's control characters, such as a newline, could otherwise pass for a line of the prompt.
    const name = path.replace(/\p{Cc}/gu, "?");
    const words = text.replace(/^\n+|\s+$/g, "").split("\n").map((line) => `> ${line}`);
    const heading = where === "Lines" ? name : `${name}, ${where.charAt(0).toLowerCase()}${where.slice(1)}`;
    return [`${heading}:`, ...words].join("\n");
  });
  return ["Review comments on your changes:", ...quoted].join("\n\n");
}
