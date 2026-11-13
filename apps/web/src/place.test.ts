import assert from "node:assert/strict";
import { test } from "node:test";

import { isId, placeOf } from "./place.ts";

test("a chat is named by a workspace and a chat in the query", () => {
  assert.deepEqual(placeOf("?server=x&workspace=w-1&chat=c_2"), { workspace: "w-1", chat: "c_2" });
  assert.equal(placeOf("?workspace=w-1"), undefined);
  assert.equal(placeOf("?chat=c-1"), undefined);
  assert.equal(placeOf("?workspace=w-1&chat=--help%20me"), undefined);
  assert.equal(placeOf("?workspace=w%2F1&chat=c-1"), undefined);
});

test("ids are 1 to 64 ASCII letters, digits, dashes or underscores", () => {
  assert.equal(isId("aZ09-_"), true);
  assert.equal(isId("a".repeat(64)), true);
  assert.equal(isId("a".repeat(65)), false);
  assert.equal(isId(""), false);
  assert.equal(isId("é"), false);
  assert.equal(isId("a b"), false);
});
