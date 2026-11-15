import assert from "node:assert/strict";
import { test } from "node:test";

import type { SearchHit } from "../protocol.ts";
import { listed, opensPalette, parts } from "./hits.ts";

const snippet = { text: "the plan", marks: [] };

test("hits list in the server's order, each opening its place, and a newer kind is left out", () => {
  const hits = [
    { kind: "doc", doc: "d-1", title: "Plan", snippet },
    { kind: "symbol", name: "a::b" },
    { kind: "chat", chat: "c-1", title: "Fix the build", seq: 3, snippet },
    { kind: "chat", chat: "c-1", title: "Fix the build", snippet },
    { kind: "task", task: "t-1", title: "Ship", snippet },
  ] as unknown as SearchHit[];

  assert.deepEqual(
    listed(hits).map(({ key, kind, title, place }) => [key, kind, title, place]),
    [
      ["doc:d-1", "Doc", "Plan", { view: "docs", doc: "d-1" }],
      ["chat:c-1:3", "Chat", "Fix the build", { view: "chats", chat: "c-1", at: 3 }],
      ["chat:c-1:", "Chat", "Fix the build", { view: "chats", chat: "c-1" }],
      ["task:t-1", "Task", "Ship", { view: "tasks", task: "t-1" }],
    ],
  );
});

test("a snippet cuts at its marks, and marks out of order or reversed are skipped", () => {
  assert.deepEqual(parts({ text: "the plan for 🚀 launch", marks: [{ start: 4, end: 8 }, { start: 13, end: 15 }] }), [
    { text: "the ", marked: false },
    { text: "plan", marked: true },
    { text: " for ", marked: false },
    { text: "🚀", marked: true },
    { text: " launch", marked: false },
  ]);
  assert.deepEqual(parts({ text: "plan", marks: [{ start: 0, end: 4 }] }), [{ text: "plan", marked: true }]);
  assert.deepEqual(parts({ text: "plan", marks: [{ start: 2, end: 4 }, { start: 0, end: 1 }, { start: 3, end: 2 }] }), [
    { text: "pl", marked: false },
    { text: "an", marked: true },
  ]);
});

test("⌘K and Ctrl+K open the palette, and K alone doesn't", () => {
  assert.equal(opensPalette({ key: "k", metaKey: true, ctrlKey: false }), true);
  assert.equal(opensPalette({ key: "K", metaKey: false, ctrlKey: true }), true);
  assert.equal(opensPalette({ key: "k", metaKey: false, ctrlKey: false }), false);
  assert.equal(opensPalette({ key: "j", metaKey: true, ctrlKey: false }), false);
});
