import assert from "node:assert/strict";
import { test } from "node:test";

import { afterSend } from "./draft.ts";

test("a sent draft is cleared, and one edited while it was sent is kept", () => {
  assert.equal(afterSend("Make it blue", "Make it blue"), "");
  assert.equal(afterSend("Make it blue, and wider", "Make it blue"), "Make it blue, and wider");
  assert.equal(afterSend("", "Make it blue"), "");
});
