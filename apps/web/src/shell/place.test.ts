import assert from "node:assert/strict";
import { test } from "node:test";

import { type Place, followsHere, placeOf, searchOf } from "./place.ts";

test("the address names a chat, the tasks, the docs or one doc, or the proposals, and back again", () => {
  const places: [Place, string][] = [
    [{ view: "chats" }, "?workspace=w-1"],
    [{ view: "chats", chat: "c-1" }, "?workspace=w-1&chat=c-1"],
    [{ view: "chats", chat: "c-1", at: 12 }, "?workspace=w-1&chat=c-1&at=12"],
    [{ view: "chats", task: "t-1" }, "?workspace=w-1&task=t-1"],
    [{ view: "tasks" }, "?workspace=w-1&view=tasks"],
    [{ view: "tasks", task: "t-1" }, "?workspace=w-1&view=tasks&task=t-1"],
    [{ view: "docs" }, "?workspace=w-1&view=docs"],
    [{ view: "docs", doc: "d-1" }, "?workspace=w-1&view=docs&doc=d-1"],
    [{ view: "proposals" }, "?workspace=w-1&view=proposals"],
  ];

  for (const [place, search] of places) {
    assert.equal(searchOf("w-1", place), search);
    assert.deepEqual(placeOf(search), place);
  }
  assert.equal(searchOf(undefined, { view: "tasks" }), "");
});

test("a view or id the app doesn't know, or one in the wrong view, is left out", () => {
  assert.deepEqual(placeOf("?workspace=w-1&view=elsewhere&chat=c-1"), { view: "chats", chat: "c-1" });
  assert.deepEqual(placeOf("?workspace=w-1&chat=../x"), { view: "chats" });
  assert.deepEqual(placeOf("?workspace=w-1&view=docs&doc=a%20b"), { view: "docs" });
  assert.deepEqual(placeOf("?workspace=w-1&view=tasks&doc=d-1&chat=c-1"), { view: "tasks" });
  for (const at of ["0", "-3", "1.5", "x", "", "9007199254740993"]) {
    assert.deepEqual(placeOf(`?workspace=w-1&chat=c-1&at=${at}`), { view: "chats", chat: "c-1" });
  }
  assert.deepEqual(placeOf("?workspace=w-1&at=3"), { view: "chats" });
  assert.deepEqual(placeOf("?workspace=w-1&chat=c-1&task=t-1"), { view: "chats", chat: "c-1" });
  assert.deepEqual(placeOf("?workspace=w-1&task=../t"), { view: "chats" });
  assert.deepEqual(placeOf("?workspace=w-1&view=docs&task=t-1"), { view: "docs" });
  assert.equal(searchOf("w-1", { view: "chats", task: "t-1", at: 3 }), "?workspace=w-1&task=t-1");
  assert.deepEqual(placeOf(""), { view: "chats" });
});

test("only a plain left click on a link is followed in the page", () => {
  const plain = { button: 0, metaKey: false, ctrlKey: false, shiftKey: false, altKey: false };

  assert.equal(followsHere(plain), true);
  for (const held of ["metaKey", "ctrlKey", "shiftKey", "altKey"] as const) {
    assert.equal(followsHere({ ...plain, [held]: true }), false, held);
  }
  assert.equal(followsHere({ ...plain, button: 1 }), false);
});
