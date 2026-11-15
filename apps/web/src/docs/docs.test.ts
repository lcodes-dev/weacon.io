import assert from "node:assert/strict";
import { test } from "node:test";

import { byline } from "./docs.ts";

test("a doc's byline names who made its revision, and who accepted an agent's", () => {
  const names = new Map([
    ["m-1", "ada"],
    ["m-2", "bo"],
  ]);
  const agent = { kind: "claude", owner: { kind: "member", member: "m-1" }, machine: "laptop" } as const;

  assert.equal(byline({ version: 2, author: { kind: "person", member: "m-1" }, accepted_by: "m-1", at: 1 }, names), "Revision 2 by ada");
  assert.equal(
    byline({ version: 3, author: { kind: "agent", owner: "m-1", agent }, accepted_by: "m-2", at: 1 }, names),
    "Revision 3 by ada's Claude Code · laptop, accepted by bo",
  );
  assert.equal(byline({ version: 1, author: { kind: "person", member: "m-1" }, accepted_by: "m-9", at: 1 }, names), "Revision 1 by ada, accepted by a member");
  assert.equal(byline(undefined, names), undefined);
});
