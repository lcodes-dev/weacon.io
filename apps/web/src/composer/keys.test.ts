import assert from "node:assert/strict";
import { test } from "node:test";

import { type Key, sends } from "./keys.ts";

const enter: Key = { key: "Enter", shiftKey: false, isComposing: false, keyCode: 13 };

test("Enter sends, Shift+Enter and other keys don't", () => {
  assert.equal(sends(enter), true);
  assert.equal(sends({ ...enter, shiftKey: true }), false);
  assert.equal(sends({ ...enter, key: "a", keyCode: 65 }), false);
});

test("an Enter that belongs to a composition never sends", () => {
  assert.equal(sends({ ...enter, isComposing: true }), false);
  assert.equal(sends({ ...enter, keyCode: 229 }), false);
});
