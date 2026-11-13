import assert from "node:assert/strict";
import { test } from "node:test";

import { atBottom, selectingIn } from "./follow.ts";

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
