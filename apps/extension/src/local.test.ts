import assert from "node:assert/strict";
import { test } from "node:test";

import { isLocal } from "./local.ts";

test("only pages from this machine are local", () => {
  for (const url of [
    "http://localhost:3000/",
    "https://localhost/a?b",
    "http://127.0.0.1:8080/",
    "http://[::1]:5173/",
    "http://app.localhost/",
  ]) {
    assert.ok(isLocal(url), url);
  }
  for (const url of [
    "https://example.com/",
    "http://localhost.example.com/",
    "http://127.0.0.2/",
    "file:///home/ada/index.html",
    "chrome://extensions/",
    "not a url",
  ]) {
    assert.ok(!isLocal(url), url);
  }
});
