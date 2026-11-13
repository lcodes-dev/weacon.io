import assert from "node:assert/strict";
import { test } from "node:test";

import type { DiffContent, FileChange } from "../protocol.ts";
import { NO_CHANGES, ask, rows } from "./changes.ts";
import { changeText, contentText, hunkText, linesText, summary } from "./labels.ts";

test("the summary says how far the changes have come", () => {
  const { changes } = ask(5, "q-1", "w-1", "c-1");
  const files = (count: number) => Array.from({ length: count }, () => ({ path: "a", change: { kind: "added" }, content: { kind: "binary" } }) as const);

  assert.equal(summary(NO_CHANGES), "The changes couldn't load.");
  assert.equal(summary(changes), "Loading changes.");
  assert.equal(summary({ ...changes, cut: true }), "The changes couldn't load.");
  assert.equal(summary({ ...changes, total: 0 }), "No changes.");
  assert.equal(summary({ ...changes, files: files(1), total: 1 }), "1 file changed");
  assert.equal(summary({ ...changes, files: files(3), total: 3 }), "3 files changed");
  assert.equal(summary({ ...changes, files: files(1), total: 3 }), "3 files changed, 1 loaded");
  assert.equal(summary({ ...changes, files: files(1), total: 3, cut: true }), "3 files changed, only 1 loaded");
});

test("files say how they changed, and why they show no lines when they don't", () => {
  const changes: [FileChange, string][] = [
    [{ kind: "added" }, "Added"],
    [{ kind: "removed" }, "Removed"],
    [{ kind: "modified" }, "Modified"],
    [{ kind: "renamed", from: "src/a.rs" }, "Renamed from src/a.rs"],
    [{ kind: "copied" } as unknown as FileChange, "Changed"],
  ];
  const contents: [DiffContent, string | undefined][] = [
    [{ kind: "text", hunks: [{ old_start: 1, new_start: 1, lines: [] }] }, undefined],
    [{ kind: "text", hunks: [] }, "Only the file's mode changed."],
    [{ kind: "binary" }, "Binary file."],
    [{ kind: "too_large" }, "Too large to show."],
    [{ kind: "symlink" } as unknown as DiffContent, "This app can't show this file."],
  ];

  for (const [change, text] of changes) {
    assert.equal(changeText(change), text);
  }
  for (const [content, text] of contents) {
    assert.equal(contentText(content), text);
  }
});

test("a hunk names where it starts on each side that has lines", () => {
  assert.equal(hunkText(41, 43), "From old line 41, new line 43");
  assert.equal(hunkText(0, 1), "From new line 1");
  assert.equal(hunkText(1, 0), "From old line 1");
});

test("a range is named by its lines' numbers, old ones where only the old side has the line", () => {
  const lines = rows([
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
  ]);

  assert.equal(linesText(lines, 1, 1), "Line 12");
  assert.equal(linesText(lines, 1, 4), "Lines 12 to 13");
  assert.equal(linesText(lines, 2, 3), "Lines old 11 to old 12");
  assert.equal(linesText(lines, 2, 4), "Lines old 11 to new 13");
  assert.equal(linesText(lines, 0, 1), "Lines");
});
