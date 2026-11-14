import assert from "node:assert/strict";
import { test } from "node:test";

import { fakeServerUrl } from "./server.ts";

test("only a page opened from a file reaches the fake server it names", () => {
  assert.equal(fakeServerUrl({ protocol: "file:", search: "?server=ws%3A%2F%2F127.0.0.1%3A4100%2F" }), "ws://127.0.0.1:4100/");
  assert.equal(fakeServerUrl({ protocol: "file:", search: "" }), undefined);
  assert.equal(fakeServerUrl({ protocol: "https:", search: "?server=wss%3A%2F%2Fother.example%2F" }), undefined);
  assert.equal(fakeServerUrl({ protocol: "http:", search: "" }), undefined);
});
