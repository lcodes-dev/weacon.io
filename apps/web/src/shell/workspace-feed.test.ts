import assert from "node:assert/strict";
import { test } from "node:test";

import { feedUrl, retryAfter } from "./workspace-feed.ts";

test("a workspace's feed is on this page's own server, over TLS when the page is, after a place", () => {
  assert.equal(feedUrl({ protocol: "https:", host: "weacon.example" }, "ws-1", 0), "wss://weacon.example/api/workspaces/ws-1/feed?since=0");
  assert.equal(feedUrl({ protocol: "http:", host: "127.0.0.1:8080" }, "ws-2", 42), "ws://127.0.0.1:8080/api/workspaces/ws-2/feed?since=42");
});

test("the feed is opened again after a wait that doubles, up to half a minute", () => {
  assert.deepEqual([0, 1, 2, 3, 5, 6, 7, 40].map(retryAfter), [1_000, 1_000, 2_000, 4_000, 16_000, 30_000, 30_000, 30_000]);
});
