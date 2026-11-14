import type { Comment } from "./comments.ts";

/**
 * The prompt that sends `comments` to the chat's agent: each comment's file, side and lines, as
 * they were when it was made, then its words quoted. The agent reads the lines from the file itself.
 */
export function reviewPrompt(comments: readonly Comment[]): string {
  const quoted = comments.map(({ path, where, text }) => {
    // A file name's control characters, such as a newline, could otherwise pass for a line of the prompt.
    const name = path.replace(/\p{Cc}/gu, "?");
    const words = text.replace(/^\n+|\s+$/g, "").split("\n").map((line) => `> ${line}`);
    const heading = where === "Lines" ? name : `${name}, ${where.charAt(0).toLowerCase()}${where.slice(1)}`;
    return [`${heading}:`, ...words].join("\n");
  });
  return ["Review comments on your changes:", ...quoted].join("\n\n");
}
