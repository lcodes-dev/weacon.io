import assert from "node:assert/strict";
import { test } from "node:test";

import { blocks, inlines } from "./markdown.ts";

test("inline code, bold and italics are marked, and everything else stays text", () => {
  assert.deepEqual(inlines("Run `cargo test`, **now**, *please* or _later_; 2 * 3 * 4 and a_b_c."), [
    { kind: "text", text: "Run " },
    { kind: "code", text: "cargo test" },
    { kind: "text", text: ", " },
    { kind: "strong", text: "now" },
    { kind: "text", text: ", " },
    { kind: "em", text: "please" },
    { kind: "text", text: " or _later_; 2 * 3 * 4 and a_b_c." },
  ]);
  assert.deepEqual(inlines("<script>alert(1)</script>"), [{ kind: "text", text: "<script>alert(1)</script>" }]);
});

test("paragraphs, headings, lists and code blocks are read in order", () => {
  const source = [
    "## The fix",
    "",
    "Units now convert",
    "before merging.",
    "- cups",
    "- `tsp`",
    "",
    "1. one",
    "2. two",
    "```rust",
    "fn main() {}",
    "",
    "```",
    "Done.",
  ].join("\n");

  assert.deepEqual(blocks(source), [
    { kind: "heading", inlines: [{ kind: "text", text: "The fix" }] },
    { kind: "paragraph", inlines: [{ kind: "text", text: "Units now convert\nbefore merging." }] },
    { kind: "list", items: [[{ kind: "text", text: "cups" }], [{ kind: "code", text: "tsp" }]] },
    { kind: "list", start: 1, items: [[{ kind: "text", text: "one" }], [{ kind: "text", text: "two" }]] },
    { kind: "code", text: "fn main() {}\n" },
    { kind: "paragraph", inlines: [{ kind: "text", text: "Done." }] },
  ]);
});

test("an unclosed code block runs to the end", () => {
  assert.deepEqual(blocks("```\nlet a = 1;"), [{ kind: "code", text: "let a = 1;" }]);
  assert.deepEqual(blocks(""), []);
});

test("asterisks that don't hug their text stay text", () => {
  assert.deepEqual(inlines("char *p, *q and 2 ** 3 ** 4"), [{ kind: "text", text: "char *p, *q and 2 ** 3 ** 4" }]);
  assert.deepEqual(inlines("**a** *b*"), [
    { kind: "strong", text: "a" },
    { kind: "text", text: " " },
    { kind: "em", text: "b" },
  ]);
});

test("a numbered list split by blank lines keeps its numbers", () => {
  assert.deepEqual(blocks("1. a\n\n2. b"), [
    { kind: "list", start: 1, items: [[{ kind: "text", text: "a" }]] },
    { kind: "list", start: 2, items: [[{ kind: "text", text: "b" }]] },
  ]);
});

test("large inputs parse in linear time", () => {
  const began = performance.now();
  blocks("a\n\n".repeat(80_000));
  blocks("```\n".repeat(80_000));
  blocks("- a\n".repeat(80_000));

  assert.ok(performance.now() - began < 1_000, "parsing took too long");
});
