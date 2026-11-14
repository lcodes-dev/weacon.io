import assert from "node:assert/strict";
import { test } from "node:test";

import { chatRefOf, workspaceOf } from "./chat-ref.ts";

test("a chat is named by a workspace and a chat in the query", () => {
  assert.deepEqual(chatRefOf("?server=x&workspace=w-1&chat=c_2"), { workspace: "w-1", chat: "c_2" });
  assert.equal(chatRefOf("?workspace=w-1"), undefined);
  assert.equal(chatRefOf("?chat=c-1"), undefined);
  assert.equal(chatRefOf("?workspace=w-1&chat=--help%20me"), undefined);
  assert.equal(chatRefOf("?workspace=w%2F1&chat=c-1"), undefined);
});

test("a workspace alone is named by a well-formed `?workspace=`", () => {
  assert.equal(workspaceOf("?workspace=w-1"), "w-1");
  assert.equal(workspaceOf("?chat=c-1&workspace=w_2"), "w_2");
  assert.equal(workspaceOf("?workspace=a%2Fb"), undefined);
  assert.equal(workspaceOf("?workspace="), undefined);
  assert.equal(workspaceOf(""), undefined);
});
