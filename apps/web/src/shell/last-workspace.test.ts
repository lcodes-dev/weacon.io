import assert from "node:assert/strict";
import { test } from "node:test";

import { type Store, type Storing, opening, remember } from "./last-workspace.ts";

function store(): Storing {
  const kept = new Map<string, string>();
  const methods: Store = {
    getItem: (key) => kept.get(key) ?? null,
    setItem: (key, value) => void kept.set(key, value),
    removeItem: (key) => void kept.delete(key),
  };
  return () => methods;
}

const failing: Store = {
  getItem: () => {
    throw new Error("off");
  },
  setItem: () => {
    throw new Error("full");
  },
  removeItem: () => {
    throw new Error("off");
  },
};

test("the page opens the workspace it names, or else the last one opened here", () => {
  const kept = store();
  assert.equal(opening("", kept), undefined);
  remember(kept, "ws-1");
  assert.equal(opening("", kept), "ws-1");
  assert.equal(opening("?workspace=ws-2", kept), "ws-2");
  remember(kept, "ws-1");
  assert.equal(opening("?workspace=a/b", kept), "ws-1");
  remember(kept, undefined);
  assert.equal(opening("", kept), undefined);
});

test("a kept id of the wrong form is ignored, and storage that fails is no failure", () => {
  const kept = store();
  remember(kept, "a/b");
  assert.equal(opening("", kept), undefined);
  const blocked: Storing = () => {
    throw new Error("blocked");
  };
  for (const broken of [() => failing, blocked]) {
    assert.doesNotThrow(() => remember(broken, "ws-1"));
    assert.doesNotThrow(() => remember(broken, undefined));
    assert.equal(opening("?workspace=ws-2", broken), "ws-2");
    assert.equal(opening("", broken), undefined);
  }
});
