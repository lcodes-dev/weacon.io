import assert from "node:assert/strict";
import { test } from "node:test";

import { type SelectorNode, cssSelector } from "./selector.ts";

type Fake = { id: string; localName: string; parentElement: Fake | null; children: Fake[] };

function tree(localName: string, id: string, ...children: Fake[]): Fake {
  const node: Fake = { id, localName, parentElement: null, children };
  for (const child of children) {
    child.parentElement = node;
  }
  return node;
}

test("a selector climbs to the nearest plain id, numbering same-tag siblings", () => {
  const save = tree("button", "");
  const list = tree("ul", "", tree("li", ""), tree("li", "", tree("a", "")), tree("li", ""));
  const main = tree("main", "app", tree("p", ""), save, list);
  const html = tree("html", "", tree("body", "", tree("div", "1-not-plain", main), tree("div", "")));
  const at = (...path: number[]): SelectorNode => path.reduce<Fake>((node, index) => node.children[index] as Fake, html);

  assert.equal(cssSelector(save), "#app > button");
  assert.equal(cssSelector(at(0, 0, 0, 2, 1, 0)), "#app > ul > li:nth-of-type(2) > a");
  assert.equal(cssSelector(at(0, 0)), "html > body > div:nth-of-type(1)");
  assert.equal(cssSelector(main), "#app");
  assert.equal(cssSelector(html), "html");
});
