import assert from "node:assert/strict";
import { test } from "node:test";

import { componentOf } from "./component.ts";

// A page whose one element is `element`; the selector it was asked for is kept.
function page(element: object | null) {
  const asked: string[] = [];
  return {
    asked,
    root: {
      querySelector: (selector: string) => {
        asked.push(selector);
        return element as Element | null;
      },
    },
  };
}

test("react's development fiber names the nearest component and its file", () => {
  function SaveButton() {}
  const host = { type: "button", return: { type: SaveButton, _debugSource: { fileName: "src/save.tsx" } } };
  const named = { type: "div", return: { type: { displayName: "Memo(Card)" } } };
  const { root, asked } = page({ "__reactFiber$x1": host });

  assert.deepEqual(componentOf("main > button", root), { component: "SaveButton", source: "src/save.tsx" });
  assert.deepEqual(asked, ["main > button"]);
  assert.deepEqual(componentOf("div", page({ "__reactFiber$y": named }).root), { component: "Memo(Card)" });
  assert.equal(componentOf("p", page({ "__reactFiber$z": { type: "p", return: null } }).root), null);
});

test("vue names its component and file, in version 3 or 2", () => {
  const three = { __vueParentComponent: { type: { __name: "Cart", __file: "src/Cart.vue" } } };
  const named = { __vueParentComponent: { type: { name: "Basket" } } };
  const two = { __vue__: { $options: { name: "Total", __file: "src/Total.vue" } } };

  assert.deepEqual(componentOf("x", page(three).root), { component: "Cart", source: "src/Cart.vue" });
  assert.deepEqual(componentOf("x", page(named).root), { component: "Basket" });
  assert.deepEqual(componentOf("x", page(two).root), { component: "Total", source: "src/Total.vue" });
  assert.deepEqual(componentOf("x", page({ __vue__: { $options: { __file: "a.vue" } } }).root), { source: "a.vue" });
});

test("a page without a framework, or a missing element, names nothing, and long names are cut", () => {
  const long = { __vueParentComponent: { type: { __name: "n".repeat(300), __file: "" } } };
  // The component `hosts` host elements above the picked one.
  const above = (hosts: number) => {
    let fiber: object = { type: function Deep() {}, return: null };
    for (let at = 0; at < hosts; at += 1) {
      fiber = { type: "div", return: fiber };
    }
    return page({ "__reactFiber$d": fiber }).root;
  };

  assert.equal(componentOf("x", page(null).root), null);
  assert.equal(componentOf("x", page({}).root), null);
  assert.equal(componentOf("x", page({ __vueParentComponent: { type: { __name: 3 } } }).root), null);
  assert.deepEqual(componentOf("x", page(long).root), { component: "n".repeat(256) });
  assert.deepEqual(componentOf("x", above(63)), { component: "Deep" });
  assert.equal(componentOf("x", above(64)), null);
});
