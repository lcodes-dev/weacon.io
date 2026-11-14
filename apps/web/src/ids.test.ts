import assert from "node:assert/strict";
import { test } from "node:test";

import { isId, newRequestId, requestId } from "./ids.ts";

test("ids are 1 to 64 ASCII letters, digits, dashes or underscores", () => {
  assert.equal(isId("aZ09-_"), true);
  assert.equal(isId("a".repeat(64)), true);
  assert.equal(isId("a".repeat(65)), false);
  assert.equal(isId(""), false);
  assert.equal(isId("é"), false);
  assert.equal(isId("a b"), false);
});

test("a request id is its bytes in hex, and a well-formed id; a new one is random", () => {
  const id = requestId(Uint8Array.from({ length: 16 }, (_, at) => at * 17));
  assert.equal(id, "00112233445566778899aabbccddeeff");
  assert.equal(isId(id), true);
  assert.match(newRequestId(), /^[0-9a-f]{32}$/);
  assert.notEqual(newRequestId(), newRequestId());
});
