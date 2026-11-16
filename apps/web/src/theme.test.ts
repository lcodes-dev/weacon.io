import assert from "node:assert/strict";
import { test } from "node:test";

import type { Store, Storing } from "./shell/last-workspace.ts";
import { keepTheme, keptTheme } from "./theme.ts";

function store(): Storing {
  const kept = new Map<string, string>();
  const methods: Store = {
    getItem: (key) => kept.get(key) ?? null,
    setItem: (key, value) => void kept.set(key, value),
    removeItem: (key) => void kept.delete(key),
  };
  return () => methods;
}

test("the page is dark until the person picks light, and keeps their pick", () => {
  const kept = store();
  assert.equal(keptTheme(kept), "dark");
  keepTheme(kept, "light");
  assert.equal(keptTheme(kept), "light");
  keepTheme(kept, "dark");
  assert.equal(keptTheme(kept), "dark");
});

test("a kept value of another form, or storage that fails, means dark", () => {
  const kept = store();
  kept().setItem("weacon.theme", "sepia");
  assert.equal(keptTheme(kept), "dark");
  const failing: Storing = () => {
    throw new Error("off");
  };
  assert.equal(keptTheme(failing), "dark");
  keepTheme(failing, "light");
});
