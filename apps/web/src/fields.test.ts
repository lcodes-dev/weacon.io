import assert from "node:assert/strict";
import { test } from "node:test";

import { record } from "./fields.ts";

test("only a JSON object has fields", () => {
  assert.deepEqual(record({ a: 1 }), { a: 1 });
  for (const value of [null, undefined, [1], "x", 3, true]) {
    assert.equal(record(value), undefined, JSON.stringify(value));
  }
});
