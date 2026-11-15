/** One line of a comparison: in both texts, only in the later one, or only in the earlier one. */
export type DiffLine = { kind: "same" | "added" | "removed"; text: string };

// Past this many line pairs between the common ends, the middle shows as wholly replaced.
const MOST_PAIRS = 1_000_000;

/** How `after` differs from `before`, line by line, with the fewest lines added and removed. */
export function lineDiff(before: string, after: string): DiffLine[] {
  const old = splitLines(before);
  const now = splitLines(after);
  let start = 0;
  while (start < old.length && start < now.length && old[start] === now[start]) {
    start++;
  }
  let end = 0;
  while (end < old.length - start && end < now.length - start && old[old.length - 1 - end] === now[now.length - 1 - end]) {
    end++;
  }
  const same = (lines: string[]): DiffLine[] => lines.map((text) => ({ kind: "same", text }));
  const middle = middleDiff(old.slice(start, old.length - end), now.slice(start, now.length - end));
  return [...same(old.slice(0, start)), ...middle, ...same(old.slice(old.length - end))];
}

function splitLines(text: string): string[] {
  return text === "" ? [] : text.replace(/\n$/, "").split("\n");
}

function middleDiff(old: string[], now: string[]): DiffLine[] {
  const removed = (text: string): DiffLine => ({ kind: "removed", text });
  const added = (text: string): DiffLine => ({ kind: "added", text });
  if (old.length * now.length > MOST_PAIRS) {
    return [...old.map(removed), ...now.map(added)];
  }
  // longest[i][j]: the longest run the two have in common from old[i] and now[j] on.
  const longest = Array.from({ length: old.length + 1 }, () => new Array<number>(now.length + 1).fill(0));
  for (let i = old.length - 1; i >= 0; i--) {
    for (let j = now.length - 1; j >= 0; j--) {
      const row = longest[i] ?? [];
      row[j] = old[i] === now[j] ? (longest[i + 1]?.[j + 1] ?? 0) + 1 : Math.max(longest[i + 1]?.[j] ?? 0, row[j + 1] ?? 0);
    }
  }
  const out: DiffLine[] = [];
  let i = 0;
  let j = 0;
  while (i < old.length || j < now.length) {
    const left = old[i];
    const right = now[j];
    if (left !== undefined && left === right) {
      out.push({ kind: "same", text: left });
      i++;
      j++;
    } else if (right !== undefined && (left === undefined || (longest[i]?.[j + 1] ?? 0) > (longest[i + 1]?.[j] ?? 0))) {
      out.push(added(right));
      j++;
    } else if (left !== undefined) {
      out.push(removed(left));
      i++;
    }
  }
  return out;
}
