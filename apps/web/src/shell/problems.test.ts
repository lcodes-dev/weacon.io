import assert from "node:assert/strict";
import { test } from "node:test";

import { MALFORMED, UNREACHABLE } from "./api.ts";
import { problemText } from "./problems.ts";

test("each failure reads as a plain sentence, and unknown ones as a general line", () => {
  assert.equal(problemText("sign_in.failed"), "That code didn't work. Check it, or ask for a new one.");
  assert.equal(problemText("sign_in.too_many"), "Too many tries. Wait a while, then try again.");
  assert.equal(problemText(UNREACHABLE), "The server can't be reached");
  assert.equal(problemText("workspace.name_invalid"), "Give it a name of one short line.");
  assert.equal(problemText("workspace.too_many"), "You can't make more workspaces.");
  assert.equal(problemText("store.full"), "Something went wrong. Try again.");
  assert.equal(problemText("constructor"), "Something went wrong. Try again.");
  assert.equal(problemText(MALFORMED), "Something went wrong. Try again.");
});
