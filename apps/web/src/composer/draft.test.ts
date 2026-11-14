import assert from "node:assert/strict";
import { test } from "node:test";

import { type Draft, EMPTY_DRAFT, type MentionItem, editDraft, pickMention, query } from "./draft.ts";

const jun: MentionItem = { kind: "person", id: "m-1", label: "Jun Sato" };
const codex: MentionItem = { kind: "agent", id: "a-1", label: "Codex" };

// "hi @Jun Sato and @Codex ok"
const both: Draft = {
  text: "hi @Jun Sato and @Codex ok",
  mentions: [
    { start: 3, end: 12, item: jun },
    { start: 17, end: 23, item: codex },
  ],
};

test("typing keeps mentions, shifting those after the edit", () => {
  assert.deepEqual(editDraft(EMPTY_DRAFT, "a", 1), { draft: { text: "a", mentions: [] }, caret: 1 });
  assert.deepEqual(editDraft({ text: "aa", mentions: [] }, "aaa", 2), { draft: { text: "aaa", mentions: [] }, caret: 2 });

  const typed = editDraft(both, "oh hi @Jun Sato and @Codex ok", 3);
  assert.equal(typed.draft.text, "oh hi @Jun Sato and @Codex ok");
  assert.deepEqual(
    typed.draft.mentions.map(({ start, end }) => [start, end]),
    [
      [6, 15],
      [20, 26],
    ],
  );
  assert.equal(typed.caret, 3);

  const between = editDraft(both, "hi @Jun Sato, and @Codex ok", 13);
  assert.deepEqual(
    between.draft.mentions.map(({ start, end }) => [start, end]),
    [
      [3, 12],
      [18, 24],
    ],
  );
});

test("an edit that reaches into a mention removes all of it", () => {
  // Backspace at the end of the first mention.
  assert.deepEqual(editDraft(both, "hi @Jun Sat and @Codex ok", 11), {
    draft: { text: "hi  and @Codex ok", mentions: [{ start: 8, end: 14, item: codex }] },
    caret: 3,
  });
  // Typing inside the second.
  assert.deepEqual(editDraft(both, "hi @Jun Sato and @Coxdex ok", 20), {
    draft: { text: "hi @Jun Sato and x ok", mentions: [{ start: 3, end: 12, item: jun }] },
    caret: 18,
  });
  // A selection across both, replaced.
  assert.deepEqual(editDraft(both, "hi z ok", 4), { draft: { text: "hi z ok", mentions: [] }, caret: 4 });
});

test("while an input method composes, a mention it reaches into becomes plain text", () => {
  assert.deepEqual(editDraft(both, "hi @Jun Sあato and @Codex ok", 10, true), {
    draft: { text: "hi @Jun Sあato and @Codex ok", mentions: [{ start: 18, end: 24, item: codex }] },
    caret: 10,
  });
});

test("typing right at a mention's edges leaves it whole", () => {
  const after = editDraft(both, "hi @Jun Sato! and @Codex ok", 13);
  assert.deepEqual(after.draft.mentions[0], { start: 3, end: 12, item: jun });
  const before = editDraft(both, "hi x@Jun Sato and @Codex ok", 4);
  assert.deepEqual(before.draft.mentions[0], { start: 4, end: 13, item: jun });
});

test("the query is the @ word before the caret, outside mentions", () => {
  assert.deepEqual(query({ text: "@", mentions: [] }, 1), { start: 0, text: "" });
  assert.deepEqual(query({ text: "hi @ju", mentions: [] }, 6), { start: 3, text: "ju" });
  assert.deepEqual(query({ text: "hi ＠じゅ", mentions: [] }, 6), { start: 3, text: "じゅ" });
  assert.deepEqual(query({ text: "hi @ju there", mentions: [] }, 6), { start: 3, text: "ju" });
  assert.equal(query({ text: "hi @ju there", mentions: [] }, 12), undefined);
  assert.equal(query({ text: "ada@example", mentions: [] }, 11), undefined);
  assert.equal(query(both, 7), undefined);
  assert.equal(query(both, 12), undefined);
});

test("a pick replaces the query with the mention and a space", () => {
  const draft: Draft = { text: "hi @ju, @Codex", mentions: [{ start: 8, end: 14, item: codex }] };
  assert.deepEqual(pickMention(draft, { start: 3, text: "ju" }, jun), {
    draft: {
      text: "hi @Jun Sato , @Codex",
      mentions: [
        { start: 3, end: 12, item: jun },
        { start: 15, end: 21, item: codex },
      ],
    },
    caret: 13,
  });
});
