import assert from "node:assert/strict";
import { test } from "node:test";

import { reported, title } from "./feedback.ts";

test("known failures get their own words and others a general one", () => {
  assert.equal(title("extension.page_not_local"), "Weacon picks elements on local pages only");
  assert.equal(title("pick.not_kept"), "The element couldn't be sent to Weacon");
});

test("only the extension's own failures are reported to the core", () => {
  assert.ok(reported("extension.failed"));
  assert.ok(!reported("pick.not_kept"));
  assert.ok(!reported("request.conflict"));
});
