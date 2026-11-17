import assert from "node:assert/strict";
import { test } from "node:test";

import { hex, unhex } from "./hex.ts";

test("each byte is two lowercase hex digits", () => {
  assert.equal(hex(new Uint8Array([0, 9, 10, 255])), "00090aff");
  assert.equal(hex(new Uint8Array()), "");
});

test("hex digits read back as the bytes they spell", () => {
  assert.deepEqual(unhex("00090aff"), new Uint8Array([0, 9, 10, 255]));
  assert.deepEqual(unhex(""), new Uint8Array());
  assert.equal(hex(unhex("00112233445566778899aabbccddeeff")), "00112233445566778899aabbccddeeff");
});
