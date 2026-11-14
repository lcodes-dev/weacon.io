import assert from "node:assert/strict";
import { test } from "node:test";

import { type Comment, addComment, editComment, removeComment, selectRows, span, unsent } from "./comments.ts";

test("a pick starts a selection, and an extending pick in the same file moves its end", () => {
  const one = selectRows(undefined, "a.rs", 5, false);
  const down = selectRows(one, "a.rs", 9, true);
  const up = selectRows(down, "a.rs", 2, true);

  assert.deepEqual(one, { path: "a.rs", anchor: 5, focus: 5 });
  assert.deepEqual(down, { path: "a.rs", anchor: 5, focus: 9 });
  assert.deepEqual(span(up), { first: 2, last: 5 });
  assert.deepEqual(selectRows(down, "a.rs", 7, false), { path: "a.rs", anchor: 7, focus: 7 });
  assert.deepEqual(selectRows(down, "b.rs", 7, true), { path: "b.rs", anchor: 7, focus: 7 });
});

test("comments get the next id, and are edited and removed by it", () => {
  const first = addComment([], { path: "a.rs", anchor: 9, focus: 4 }, "Why?");
  const both = addComment(first, { path: "b.rs", anchor: 1, focus: 1 }, "Fine.");
  const comment: Comment = { id: 1, path: "a.rs", first: 4, last: 9, text: "Why?" };

  assert.deepEqual(first, [comment]);
  assert.deepEqual(both[1], { id: 2, path: "b.rs", first: 1, last: 1, text: "Fine." });
  assert.equal(addComment(removeComment(both, 1), { path: "c.rs", anchor: 1, focus: 1 }, "Next.").at(-1)?.id, 3);
  assert.deepEqual(editComment(both, 1, "Why not?"), [{ ...comment, text: "Why not?" }, both[1]]);
  assert.deepEqual(removeComment(both, 1), [both[1]]);
  assert.deepEqual(removeComment(both, 3), both);
});

test("a sent review leaves the comments it didn't carry, and those edited since", () => {
  const sent: Comment[] = [
    { id: 1, path: "a.rs", first: 1, last: 2, text: "Why?" },
    { id: 2, path: "a.rs", first: 4, last: 4, text: "Rename." },
  ];
  const now: Comment[] = [
    sent[0]!,
    { ...sent[1]!, text: "Rename it to total." },
    { id: 3, path: "b.rs", first: 0, last: 0, text: "Added while sending." },
  ];

  assert.deepEqual(unsent(now, sent), now.slice(1));
  assert.deepEqual(unsent(now, []), now);
});
