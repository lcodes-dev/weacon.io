import assert from "node:assert/strict";
import { test } from "node:test";

import { type DiffLine, lineDiff } from "./line-diff.ts";

const show = (lines: DiffLine[]) => lines.map(({ kind, text }) => `${kind === "same" ? " " : kind === "added" ? "+" : "-"}${text}`);

test("a comparison keeps the lines both share and marks the rest added or removed", () => {
  assert.deepEqual(show(lineDiff("# Plan\na\nb\nc\n", "# Plan\na\nB\nc\nd\n")), [" # Plan", " a", "-b", "+B", " c", "+d"]);
  assert.deepEqual(show(lineDiff("", "one\ntwo")), ["+one", "+two"]);
  assert.deepEqual(show(lineDiff("one\ntwo\n", "")), ["-one", "-two"]);
  assert.deepEqual(show(lineDiff("same", "same\n")), [" same"]);
  assert.deepEqual(show(lineDiff("a\nx\nb\ny\nc", "a\nb\nc")), [" a", "-x", " b", "-y", " c"]);
  assert.deepEqual(show(lineDiff("x\na\nb", "a\nb\nz")), ["-x", " a", " b", "+z"]);
});

test("a middle too large to compare line by line shows as replaced", () => {
  const before = Array.from({ length: 1_001 }, (_, at) => `old ${at}`).join("\n");
  const after = Array.from({ length: 1_001 }, (_, at) => `new ${at}`).join("\n");

  const lines = lineDiff(`top\n${before}\nend`, `top\n${after}\nend`);

  assert.equal(lines.length, 2 + 2 * 1_001);
  assert.deepEqual(lines[0], { kind: "same", text: "top" });
  assert.deepEqual(lines[1], { kind: "removed", text: "old 0" });
  assert.deepEqual(lines[1_002], { kind: "added", text: "new 0" });
  assert.deepEqual(lines.at(-1), { kind: "same", text: "end" });
});
