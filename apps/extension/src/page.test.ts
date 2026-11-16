import assert from "node:assert/strict";
import { test } from "node:test";

import { isWebPage } from "./page.ts";

test("the picker works on any http or https page, and nowhere else", () => {
  for (const url of ["http://localhost:3000/", "https://example.test/cart?x=1", "http://127.0.0.1/"]) {
    assert.ok(isWebPage(url), url);
  }
  for (const url of ["chrome://extensions", "file:///tmp/a.html", "about:blank", "not a url", ""]) {
    assert.ok(!isWebPage(url), url);
  }
});
