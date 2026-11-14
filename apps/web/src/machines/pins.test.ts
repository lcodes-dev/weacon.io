import assert from "node:assert/strict";
import { test } from "node:test";

import { type PinStore, Pins } from "./pins.ts";

function memoryStore(): PinStore {
  const keys = new Map<string, string>();
  return {
    get: async (machine) => keys.get(machine),
    put: async (machine, key) => {
      keys.set(machine, key);
    },
    delete: async (machine) => {
      keys.delete(machine);
    },
  };
}

const KEY = "ab".repeat(32);
const OTHER = "cd".repeat(32);

test("a key is trusted only once pinned, and only that key", async () => {
  const pins = new Pins(memoryStore());

  assert.equal(await pins.pinned("m-1"), undefined);
  assert.equal(await pins.matches("m-1", KEY), "unpinned");
  await pins.pin("m-1", KEY);
  assert.equal(await pins.pinned("m-1"), KEY);
  assert.equal(await pins.matches("m-1", KEY), "match");
  assert.equal(await pins.matches("m-1", OTHER), "mismatch");
  assert.equal(await pins.matches("m-2", KEY), "unpinned");
});

test("pinning again replaces the key, and unpinning forgets it", async () => {
  const pins = new Pins(memoryStore());
  await pins.pin("m-1", KEY);
  await pins.pin("m-1", OTHER);

  assert.equal(await pins.matches("m-1", OTHER), "match");
  await pins.unpin("m-1");
  assert.equal(await pins.matches("m-1", OTHER), "unpinned");
});

test("only a machine key can be pinned", async () => {
  const pins = new Pins(memoryStore());

  for (const bad of ["", "AB".repeat(32), ` ${KEY}`, KEY.slice(1)]) {
    await assert.rejects(pins.pin("m-1", bad), RangeError, bad);
  }
  assert.equal(await pins.pinned("m-1"), undefined);
});
