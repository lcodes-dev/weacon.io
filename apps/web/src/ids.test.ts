import assert from "node:assert/strict";
import { test } from "node:test";

import { isId, newRequestId } from "./ids.ts";

test("ids are 1 to 64 ASCII letters, digits, dashes or underscores", () => {
  assert.equal(isId("aZ09-_"), true);
  assert.equal(isId("a".repeat(64)), true);
  assert.equal(isId("a".repeat(65)), false);
  assert.equal(isId(""), false);
  assert.equal(isId("é"), false);
  assert.equal(isId("a b"), false);
});

test("a new request id is 16 random bytes in hex, and a well-formed id", () => {
  const id = newRequestId();
  assert.match(id, /^[0-9a-f]{32}$/);
  assert.equal(isId(id), true);
  assert.notEqual(newRequestId(), newRequestId());
});
