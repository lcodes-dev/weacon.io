import assert from "node:assert/strict";
import { test } from "node:test";

import { type Key, composing, picker, sends } from "./keys.ts";

const enter: Key = { key: "Enter", shiftKey: false, isComposing: false, keyCode: 13 };

test("Enter sends, Shift+Enter and other keys don't", () => {
  assert.equal(sends(enter), true);
  assert.equal(sends({ ...enter, shiftKey: true }), false);
  assert.equal(sends({ ...enter, key: "a", keyCode: 65 }), false);
});

test("an Enter that belongs to a composition never sends", () => {
  assert.equal(sends({ ...enter, isComposing: true }), false);
});

test("a key press is part of a composition when it says so, or has key code 229", () => {
  assert.equal(composing(enter), false);
  assert.equal(composing({ ...enter, key: "a", keyCode: 65 }), false);
  assert.equal(composing({ ...enter, isComposing: true }), true);
  assert.equal(composing({ ...enter, keyCode: 229 }), true);
});

test("in the picker, Enter and Tab pick, Escape closes, and the arrows wrap around", () => {
  const key = (name: string) => ({ ...enter, key: name });
  assert.equal(picker(key("Enter"), 3, 1), "pick");
  assert.equal(picker(key("Tab"), 3, 1), "pick");
  assert.equal(picker(key("Escape"), 3, 1), "close");
  assert.equal(picker(key("ArrowDown"), 3, 1), 2);
  assert.equal(picker(key("ArrowDown"), 3, 2), 0);
  assert.equal(picker(key("ArrowUp"), 3, 1), 0);
  assert.equal(picker(key("ArrowUp"), 3, 0), 2);
  assert.equal(picker(key("a"), 3, 0), undefined);
});

test("with Shift held, keys keep their usual meaning in the picker", () => {
  assert.equal(picker({ ...enter, shiftKey: true }, 3, 1), undefined);
  assert.equal(picker({ ...enter, key: "Tab", shiftKey: true }, 3, 1), undefined);
});
