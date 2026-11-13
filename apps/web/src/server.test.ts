import assert from "node:assert/strict";
import { test } from "node:test";

import { serverUrl } from "./server.ts";

test("the server is this page's host, unless a page opened from a file names another", () => {
  assert.equal(serverUrl({ protocol: "https:", host: "weacon.example", search: "" }), "wss://weacon.example/ws");
  assert.equal(serverUrl({ protocol: "http:", host: "localhost:8080", search: "?x=1" }), "ws://localhost:8080/ws");
  assert.equal(
    serverUrl({ protocol: "https:", host: "weacon.example", search: "?server=wss%3A%2F%2Fother.example%2F" }),
    "wss://weacon.example/ws",
  );
  assert.equal(
    serverUrl({ protocol: "file:", host: "", search: "?server=ws%3A%2F%2F127.0.0.1%3A4100%2F" }),
    "ws://127.0.0.1:4100/",
  );
});
