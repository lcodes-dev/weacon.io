import assert from "node:assert/strict";
import { test } from "node:test";

import { checkKey, shownStep } from "./check-key.ts";
import { type PinStore, Pins } from "./pins.ts";

const KEY = "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a";
const OTHER = "cd".repeat(32);

function store(keys: Map<string, string>, readFails = false): PinStore {
  return {
    get: async (id) => {
      if (readFails) throw new Error("read failed");
      return keys.get(id);
    },
    put: async () => {},
    delete: async () => {},
  };
}

test("an unpinned key comes with the fingerprint the person checks, for that key alone", async () => {
  const checked = await checkKey(new Pins(store(new Map())), "mc-1", KEY);

  assert.deepEqual(checked, { kind: "unpinned", key: KEY, shown: "300c 9c96 03b9 2a4b 39ed 3958 bf92 4011" });
});

test("a pinned key is trusted or another one is, with nothing to confirm", async () => {
  const pins = new Pins(store(new Map([["mc-1", KEY]])));

  assert.deepEqual(await checkKey(pins, "mc-1", KEY), { kind: "match", key: KEY });
  assert.deepEqual(await checkKey(pins, "mc-1", OTHER), { kind: "mismatch", key: OTHER });
});

test("a store or a key that can't be checked offers nothing to confirm", async () => {
  assert.deepEqual(await checkKey(new Pins(store(new Map(), true)), "mc-1", KEY), { kind: "failed", key: KEY });
  assert.deepEqual(await checkKey(new Pins(store(new Map())), "mc-1", "not a key"), { kind: "failed", key: "not a key" });
});

test("a key the server lists after the check is never trusted or offered, whatever the check found", () => {
  const unpinned = { kind: "unpinned", key: KEY, shown: "300c" } as const;

  assert.deepEqual(shownStep(undefined, KEY), { kind: "none" });
  assert.deepEqual(shownStep({ kind: "match", key: KEY }, KEY), { kind: "none" });
  assert.deepEqual(shownStep(unpinned, KEY), { kind: "confirm", key: KEY, shown: "300c" });
  assert.deepEqual(shownStep({ kind: "mismatch", key: KEY }, KEY), { kind: "mismatch" });
  assert.deepEqual(shownStep({ kind: "failed", key: KEY }, KEY), { kind: "failed" });
  for (const checked of [{ kind: "match", key: KEY } as const, unpinned, { kind: "failed", key: KEY } as const]) {
    assert.deepEqual(shownStep(checked, OTHER), { kind: "changed" }, checked.kind);
  }
});
