import assert from "node:assert/strict";
import { test } from "node:test";

import { fromHex, hex } from "./hex.ts";

test("each byte is two lowercase hex digits", () => {
  assert.equal(hex(new Uint8Array([0, 9, 10, 255])), "00090aff");
  assert.equal(hex(new Uint8Array()), "");
});

test("lowercase hex reads back as its bytes, and nothing else reads at all", () => {
  assert.deepEqual(fromHex("00090aff"), new Uint8Array([0, 9, 10, 255]));
  assert.deepEqual(fromHex(""), new Uint8Array());
  for (const text of ["0", "0A", "zz", "00 ", "0x00"]) {
    assert.equal(fromHex(text), undefined, text);
  }
});
