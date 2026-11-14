import assert from "node:assert/strict";
import { test } from "node:test";

import { chatRefOf } from "./chat-ref.ts";

test("a chat is named by a workspace and a chat in the query", () => {
  assert.deepEqual(chatRefOf("?server=x&workspace=w-1&chat=c_2"), { workspace: "w-1", chat: "c_2" });
  assert.equal(chatRefOf("?workspace=w-1"), undefined);
  assert.equal(chatRefOf("?chat=c-1"), undefined);
  assert.equal(chatRefOf("?workspace=w-1&chat=--help%20me"), undefined);
  assert.equal(chatRefOf("?workspace=w%2F1&chat=c-1"), undefined);
});
