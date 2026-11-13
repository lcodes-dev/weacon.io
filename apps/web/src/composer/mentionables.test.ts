import assert from "node:assert/strict";
import { test } from "node:test";

import { matching, mentionables } from "./mentionables.ts";

const labels = (text: string) => matching(mentionables(), text).map(({ label }) => label);

test("an item matches when a word of its label starts with the query, in any case", () => {
  assert.deepEqual(labels("sa"), ["Jun Sato"]);
  assert.deepEqual(labels("CO"), ["Claude Code", "Codex"]);
  assert.deepEqual(labels("王"), ["王伟"]);
  assert.deepEqual(labels("x"), []);
});

test("an empty query offers the first five, people and agents alike", () => {
  const many = Array.from({ length: 7 }, (_, at) => ({ kind: "person", id: `m-${at}`, label: `P${at}` }));
  assert.deepEqual(
    matching(many, "").map(({ label }) => label),
    ["P0", "P1", "P2", "P3", "P4"],
  );
  assert.deepEqual(new Set(matching(mentionables(), "").map(({ kind }) => kind)), new Set(["person", "agent"]));
});
