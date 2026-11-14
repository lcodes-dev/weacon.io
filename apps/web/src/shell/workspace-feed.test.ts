import assert from "node:assert/strict";
import { test } from "node:test";

import { feedUrl } from "./workspace-feed.ts";

test("a workspace's feed is on this page's own server, over TLS when the page is", () => {
  assert.equal(feedUrl({ protocol: "https:", host: "weacon.example" }, "ws-1"), "wss://weacon.example/api/workspaces/ws-1/feed");
  assert.equal(feedUrl({ protocol: "http:", host: "127.0.0.1:8080" }, "ws-2"), "ws://127.0.0.1:8080/api/workspaces/ws-2/feed");
});
