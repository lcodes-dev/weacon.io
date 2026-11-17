import assert from "node:assert/strict";
import { test } from "node:test";

import { say } from "./feedback.ts";

test("known failures get their own words and others name their code", () => {
  assert.equal(say("extension.core_other_protocol"), "Update Weacon on this machine.");
  assert.equal(say("extension.page_not_web"), "Weacon picks elements on web pages only.");
  assert.equal(say("agent.signed_out"), "Something went wrong (agent.signed_out).");
});
