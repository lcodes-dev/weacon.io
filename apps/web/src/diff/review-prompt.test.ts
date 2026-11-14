import assert from "node:assert/strict";
import { test } from "node:test";

import type { DiffFile } from "../protocol.ts";
import { reviewPrompt } from "./review-prompt.ts";

const list: DiffFile = {
  path: "src/list.rs",
  change: { kind: "modified" },
  content: {
    kind: "text",
    hunks: [
      {
        old_start: 10,
        new_start: 12,
        lines: [
          { op: "context", text: "a" },
          { op: "removed", text: "b" },
          { op: "removed", text: "c" },
          { op: "added", text: "d" },
        ],
      },
    ],
  },
};
const logo: DiffFile = { path: "logo.png", change: { kind: "added" }, content: { kind: "binary" } };

test("each comment names its file, side and lines, then quotes its words", () => {
  const prompt = reviewPrompt(
    [list, logo],
    [
      { id: 1, path: "src/list.rs", first: 1, last: 1, text: "Keep this name." },
      { id: 2, path: "src/list.rs", first: 2, last: 4, text: "  Why remove these?\nThey sort the list.  " },
      { id: 3, path: "logo.png", first: 0, last: 0, text: "Smaller, please." },
    ],
  );

  assert.equal(
    prompt,
    [
      "Review comments on your changes:",
      "src/list.rs, line 12:\n> Keep this name.",
      "src/list.rs, lines old 11 to new 13:\n> Why remove these?\n> They sort the list.",
      "logo.png, lines:\n> Smaller, please.",
    ].join("\n\n"),
  );
});

test("a comment on a file no longer among the changes still names it", () => {
  assert.equal(
    reviewPrompt([], [{ id: 1, path: "gone.rs", first: 3, last: 5, text: "Bring it back." }]),
    "Review comments on your changes:\n\ngone.rs, lines:\n> Bring it back.",
  );
});
