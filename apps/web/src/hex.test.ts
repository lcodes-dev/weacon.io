import assert from "node:assert/strict";
import { test } from "node:test";

import { hex } from "./hex.ts";

test("each byte is two lowercase hex digits", () => {
  assert.equal(hex(new Uint8Array([0, 9, 10, 255])), "00090aff");
  assert.equal(hex(new Uint8Array()), "");
});
