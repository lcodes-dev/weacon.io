import assert from "node:assert/strict";
import { test } from "node:test";

import type { Comment } from "./comments.ts";
import { reviewPrompt } from "./review-prompt.ts";

const comment = (path: string, where: string, text: string): Comment => ({ id: 1, path, first: 0, last: 0, where, text });

test("each comment names its file, side and lines as they were when it was made, then quotes its words", () => {
  const prompt = reviewPrompt([
    comment("src/list.rs", "Line 12", "Keep this name."),
    comment("src/list.rs", "Lines old 11 to new 13", "Why remove these?\nThey sort the list.  "),
    comment("logo.png", "Lines", "Smaller, please."),
  ]);

  assert.equal(
    prompt,
    [
      "Review comments on your changes:",
      "src/list.rs, line 12:\n> Keep this name.",
      "src/list.rs, lines old 11 to new 13:\n> Why remove these?\n> They sort the list.",
      "logo.png:\n> Smaller, please.",
    ].join("\n\n"),
  );
});

test("a file name can't add a line of its own, and a comment keeps its indentation", () => {
  assert.equal(
    reviewPrompt([comment("a\nb.rs", "Line 3", "\n    let x = 1;\n")]),
    "Review comments on your changes:\n\na?b.rs, line 3:\n>     let x = 1;",
  );
});
