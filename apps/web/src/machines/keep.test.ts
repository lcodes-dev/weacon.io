import assert from "node:assert/strict";
import { test } from "node:test";

import type { MachineSummary, PairPending } from "../protocol.ts";
import { keep } from "./keep.ts";
import { type PinStore, Pins } from "./pins.ts";

const KEY = "ab".repeat(32);
const OTHER = "cd".repeat(32);
const SHOWN: PairPending = { pairing: "p-1", name: "Studio", os: "linux", key: KEY };
const machine = (key: string): MachineSummary => ({ id: "mc-1", name: "Studio", os: "linux", key, paired_at: 1 });

/** A store holding `keys`, whose reads or writes fail when told to. */
function store(keys: Map<string, string>, fails: { read?: boolean; write?: boolean } = {}): PinStore {
  return {
    get: async (id) => {
      if (fails.read) throw new Error("read failed");
      return keys.get(id);
    },
    put: async (id, key) => {
      if (fails.write) throw new Error("write failed");
      keys.set(id, key);
    },
    delete: async (id) => {
      keys.delete(id);
    },
  };
}

test("the key the person checked is pinned, and pinning it again changes nothing", async () => {
  const keys = new Map<string, string>();
  const pins = new Pins(store(keys));

  assert.equal(await keep(pins, machine(KEY), SHOWN), undefined);
  assert.equal(await keep(pins, machine(KEY), SHOWN), undefined);
  assert.deepEqual([...keys], [["mc-1", KEY]]);
});

test("a key other than the one checked, or than the one already pinned, is never pinned", async () => {
  const fresh = new Map<string, string>();
  const swapped = await keep(new Pins(store(fresh)), machine(OTHER), SHOWN);
  const pinned = new Map([["mc-1", OTHER]]);
  const replaced = await keep(new Pins(store(pinned)), machine(KEY), SHOWN);

  assert.match(swapped ?? "", /isn't the one you checked/);
  assert.match(replaced ?? "", /isn't the one you checked/);
  assert.deepEqual([...fresh], []);
  assert.deepEqual([...pinned], [["mc-1", OTHER]]);
});

test("a store that can't be read or written pins nothing and says so", async () => {
  const keys = new Map<string, string>();
  const unread = await keep(new Pins(store(keys, { read: true })), machine(KEY), SHOWN);
  const unwritten = await keep(new Pins(store(keys, { write: true })), machine(KEY), SHOWN);

  assert.match(unread ?? "", /couldn't check the machine's key/);
  assert.match(unwritten ?? "", /couldn't keep the machine's key/);
  assert.deepEqual([...keys], []);
});
