import assert from "node:assert/strict";
import { test } from "node:test";

import { type Place, placeOf, searchOf } from "./place.ts";

test("the address names a chat, the tasks, the docs or one doc, or the proposals, and back again", () => {
  const places: [Place, string][] = [
    [{ view: "chats" }, "?workspace=w-1"],
    [{ view: "chats", chat: "c-1" }, "?workspace=w-1&chat=c-1"],
    [{ view: "tasks" }, "?workspace=w-1&view=tasks"],
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
  assert.deepEqual(placeOf(""), { view: "chats" });
});
