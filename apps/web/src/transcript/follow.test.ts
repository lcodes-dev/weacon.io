import assert from "node:assert/strict";
import { test } from "node:test";

import { atBottom, followsAfter, leavesBottom, reachesBottom, selectingIn, touches } from "./follow.ts";

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

test("only keys that scroll up, outside an editable field, leave the bottom", () => {
  const key = (name: string, shiftKey = false) => ({ key: name, shiftKey });
  for (const name of ["ArrowUp", "PageUp", "Home"]) {
    assert.equal(leavesBottom(key(name), false), true, name);
    assert.equal(leavesBottom(key(name), true), false, name);
  }
  assert.equal(leavesBottom(key(" ", true), false), true);
  assert.equal(leavesBottom(key(" "), false), false);
  assert.equal(leavesBottom(key("ArrowDown"), false), false);
  assert.equal(leavesBottom(key("a"), false), false);
});

test("a scroll stops following only when the reader makes it away from the bottom", () => {
  // At the bottom, whoever scrolled.
  assert.equal(followsAfter(false, true, false), true);
  assert.equal(followsAfter(false, true, true), true);
  // The browser moved the view: as it was.
  assert.equal(followsAfter(true, false, false), true);
  assert.equal(followsAfter(false, false, false), false);
  // The reader moved it away.
  assert.equal(followsAfter(true, false, true), false);
});

test("a scroll down far enough to pass the bottom means to follow", () => {
  const scroll = { scrollTop: 800, scrollHeight: 2000, clientHeight: 700 };
  assert.equal(reachesBottom(1_000_000, scroll), true);
  assert.equal(reachesBottom(468, scroll), true);
  assert.equal(reachesBottom(467, scroll), false);
  assert.equal(reachesBottom(-1_000_000, scroll), false);
  assert.equal(reachesBottom(0, { scrollTop: 0, scrollHeight: 500, clientHeight: 700 }), false);
});
