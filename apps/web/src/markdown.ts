// The Markdown agents write, read into plain data; anything else stays text.

export type Inline = { kind: "text" | "code" | "strong" | "em"; text: string };

export type Block =
  | { kind: "paragraph"; inlines: Inline[] }
  | { kind: "heading"; inlines: Inline[] }
  | { kind: "list"; start?: number; items: Inline[][] }
  | { kind: "code"; text: string };

const FENCE = /^```/;
const HEADING = /^#{1,6} +(.*)$/;
const BULLET = /^[-*] +(.*)$/;
const NUMBERED = /^(\d{1,9})\. +(.*)$/;
// Underscores are left alone, as they sit inside names like `merge_items` too often.
// Markers hug their text on both sides, so `a * b * c` and `char *p, *q` keep their asterisks.
const INLINE = /(`[^`\n]+`|\*\*[^*\s](?:[^*\n]*[^*\s])?\*\*|\*[^*\s](?:[^*\n]*[^*\s])?\*)/;

export function blocks(source: string): Block[] {
  const lines = source.split("\n");
  const out: Block[] = [];
  for (let at = 0; at < lines.length; ) {
    const line = lines[at] ?? "";
    const open = fenceLength(line);
    if (open >= 3) {
      // Only a run at least as long, and nothing after it, closes the block.
      const stop = nextLine(lines, at, (next) => fenceLength(next) >= open && next.slice(fenceLength(next)).trim() === "");
      out.push({ kind: "code", text: lines.slice(at + 1, stop).join("\n") });
      at = stop + 1;
      continue;
    }
    const heading = HEADING.exec(line);
    if (heading !== null) {
      out.push({ kind: "heading", inlines: inlines(heading[1] ?? "") });
      at += 1;
      continue;
    }
    const list = listAt(lines, at);
    if (list !== undefined) {
      out.push(list.block);
      at = list.next;
      continue;
    }
    if (line.trim() === "") {
      at += 1;
      continue;
    }
    const end = paragraphEnd(lines, at);
    out.push({ kind: "paragraph", inlines: inlines(lines.slice(at, end).join("\n")) });
    at = end;
  }
  return out;
}

// The list starting at line `at`, if one does, and the line after it. A numbered list starts at
// its first number, as agents often split one list with blank lines.
function listAt(lines: readonly string[], at: number): { block: Block; next: number } | undefined {
  const numbered = NUMBERED.exec(lines[at] ?? "");
  const pattern = numbered === null ? BULLET : NUMBERED;
  const items: Inline[][] = [];
  let next = at;
  for (;;) {
    const match = pattern.exec(lines[next] ?? "");
    if (match === null) {
      break;
    }
    items.push(inlines(match.at(-1) ?? ""));
    next += 1;
  }
  if (items.length === 0) {
    return undefined;
  }
  const block: Block = numbered === null ? { kind: "list", items } : { kind: "list", start: Number(numbered[1]), items };
  return { block, next };
}

// The first line after `at` that `ends`, or the end; scanning from `at` keeps the parse linear.
function nextLine(lines: readonly string[], at: number, ends: (line: string) => boolean): number {
  let next = at + 1;
  while (next < lines.length && !ends(lines[next] ?? "")) {
    next += 1;
  }
  return next;
}

// How many backticks `line` starts with.
function fenceLength(line: string): number {
  return /^`*/.exec(line)?.[0].length ?? 0;
}

function startsBlock(line: string): boolean {
  return [FENCE, HEADING, BULLET, NUMBERED].some((pattern) => pattern.test(line));
}

// A paragraph runs until a blank line or a line that starts another kind of block.
function paragraphEnd(lines: readonly string[], at: number): number {
  return nextLine(lines, at, (next) => next.trim() === "" || startsBlock(next));
}

// `split` with a capture group puts the text between matches at even indices and matches at odd.
export function inlines(text: string): Inline[] {
  return text
    .split(INLINE)
    .map((part, at): Inline => {
      if (at % 2 === 0) {
        return { kind: "text", text: part };
      }
      if (part.startsWith("`")) {
        return { kind: "code", text: part.slice(1, -1) };
      }
      return part.startsWith("**") ? { kind: "strong", text: part.slice(2, -2) } : { kind: "em", text: part.slice(1, -1) };
    })
    .filter((part) => part.text !== "" || part.kind !== "text");
}
