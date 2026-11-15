import assert from "node:assert/strict";
import { test } from "node:test";

import type { Author } from "../protocol.ts";
import { authorText, namesOf } from "./names.ts";

test("an author is named by their member's name, and an agent by its kind, owner and machine", () => {
  const names = namesOf({ members: [{ id: "m-1", name: "ada" }] });
  const cases: [Author, string][] = [
    [{ kind: "person", member: "m-1" }, "ada"],
    [{ kind: "person", member: "m-2" }, "A member"],
    [{ kind: "agent", owner: "m-1", agent: { kind: "claude", owner: { kind: "member", member: "m-1" }, machine: "laptop" } }, "ada's Claude Code · laptop"],
    [{ kind: "agent", owner: "m-1" }, "ada's agent"],
    [{ kind: "robot" } as unknown as Author, "Someone"],
  ];

  for (const [author, expected] of cases) {
    assert.equal(authorText(author, names), expected, JSON.stringify(author));
  }
  assert.equal(namesOf(undefined).size, 0);
});
