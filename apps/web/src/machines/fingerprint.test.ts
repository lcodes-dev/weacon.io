import assert from "node:assert/strict";
import { test } from "node:test";

import { fingerprint } from "./fingerprint.ts";

test("a key's fingerprint matches what the core prints for it", async () => {
  // The core's own test key.
  const key = "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a";

  assert.equal(await fingerprint(key), "300c 9c96 03b9 2a4b 39ed 3958 bf92 4011");
  assert.equal(await fingerprint("00".repeat(32)), "6668 7aad f862 bd77 6c8f c18b 8e9f 8e20");
});

test("only a key of 64 lowercase hex digits has a fingerprint", async () => {
  for (const bad of ["", "ab".repeat(31), "AB".repeat(32), "zz".repeat(32), `${"ab".repeat(32)}0`]) {
    await assert.rejects(fingerprint(bad), RangeError, bad);
  }
});
