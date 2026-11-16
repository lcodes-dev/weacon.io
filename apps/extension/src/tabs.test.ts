import assert from "node:assert/strict";
import { test } from "node:test";

import { type Store, choose, chosen, forget } from "./tabs.ts";

function store(start?: string): Store & { value: () => string | null } {
  let value: string | null = start ?? null;
  return { getItem: () => value, setItem: (_, next) => (value = next), value: () => value };
}

test("each tab remembers its own chat, and closed tabs are forgotten", () => {
  const kept = store();
  choose(kept, 1, { workspace: "w-1", chat: "c-1" });
  choose(kept, 2, { workspace: "w-1", chat: "c-2" });
  choose(kept, 3, { workspace: "w-2", chat: "c-3" });
  forget(kept, [2, 3]);

  assert.equal(chosen(kept, 1), undefined);
  assert.deepEqual(chosen(kept, 2), { workspace: "w-1", chat: "c-2" });
  assert.deepEqual(chosen(kept, 3), { workspace: "w-2", chat: "c-3" });
  assert.equal(chosen(store(), 3), undefined);
});

test("a later choice stands, in its tab or another, whenever the closed tabs are forgotten", () => {
  const kept = store();
  choose(kept, 1, { workspace: "w-1", chat: "c-1" });
  choose(kept, 1, { workspace: "w-1", chat: "c-2" });
  choose(kept, 2, { workspace: "w-1", chat: "c-3" });
  // The forgetting each choice began ends later, and out of order.
  forget(kept, [1, 2]);
  forget(kept, [1, 2]);

  assert.deepEqual(chosen(kept, 1), { workspace: "w-1", chat: "c-2" });
  assert.deepEqual(chosen(kept, 2), { workspace: "w-1", chat: "c-3" });
});

test("what another build or a damaged store left is dropped, entry by entry", () => {
  const kept = `{"1":{"workspace":"w-1","chat":"c-1","extra":true},"2":{"workspace":3},"3":null,"4":"x"}`;

  assert.deepEqual(chosen(store(kept), 1), { workspace: "w-1", chat: "c-1" });
  for (const tab of [2, 3, 4]) {
    assert.equal(chosen(store(kept), tab), undefined);
  }
  for (const broken of ["{", "null", "3"]) {
    assert.equal(chosen(store(broken), 1), undefined);
  }
});
