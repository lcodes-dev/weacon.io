import assert from "node:assert/strict";
import { test } from "node:test";

import { atBottom, followsAfter, selectingIn, touches } from "./follow.ts";

test("the bottom is within a line of the end", () => {
  assert.equal(atBottom({ scrollTop: 900, scrollHeight: 1600, clientHeight: 700 }), true);
  assert.equal(atBottom({ scrollTop: 868, scrollHeight: 1600, clientHeight: 700 }), true);
  assert.equal(atBottom({ scrollTop: 867, scrollHeight: 1600, clientHeight: 700 }), false);
  assert.equal(atBottom({ scrollTop: 0, scrollHeight: 500, clientHeight: 700 }), true);
});

test("only a selection that reaches into the transcript holds it still", () => {
  const container = {} as Node;
  const selection = (isCollapsed: boolean, rangeCount: number, reaches: boolean) => ({
    isCollapsed,
    rangeCount,
    getRangeAt: () => ({ intersectsNode: (node: Node) => reaches && node === container }) as Range,
  });

  assert.equal(selectingIn(null, container), false);
  assert.equal(selectingIn(selection(true, 1, true), container), false);
  assert.equal(selectingIn(selection(false, 0, true), container), false);
  assert.equal(selectingIn(selection(false, 1, false), container), false);
  assert.equal(selectingIn(selection(false, 1, true), container), true);
});

test("a bare caret touches the row it sits in", () => {
  const row = {} as Node;
  const caret = (rangeCount: number, inRow: boolean) => ({
    rangeCount,
    getRangeAt: () => ({ intersectsNode: (node: Node) => inRow && node === row }) as Range,
  });

  assert.equal(touches(null, row), false);
  assert.equal(touches(caret(0, true), row), false);
  assert.equal(touches(caret(1, false), row), false);
  assert.equal(touches(caret(1, true), row), true);
});

test("away from the bottom, only a scroll up stops following; at the bottom it follows again", () => {
  assert.equal(followsAfter(true, false, true), false);
  assert.equal(followsAfter(true, false, false), true);
  assert.equal(followsAfter(false, false, false), false);
  assert.equal(followsAfter(false, true, false), true);
  assert.equal(followsAfter(true, true, true), true);
});
