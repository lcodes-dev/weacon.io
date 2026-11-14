import assert from "node:assert/strict";
import { test } from "node:test";

import type { Incoming } from "../incoming.ts";
import { failedAfter, feedUrl, heardAfter, retries, retryAfter } from "./workspace-feed.ts";

test("a workspace's feed is on this page's own server, over TLS when the page is, after a place", () => {
  assert.equal(feedUrl({ protocol: "https:", host: "weacon.example" }, "ws-1", 0), "wss://weacon.example/api/workspaces/ws-1/feed?since=0");
  assert.equal(feedUrl({ protocol: "http:", host: "127.0.0.1:8080" }, "ws-2", 42), "ws://127.0.0.1:8080/api/workspaces/ws-2/feed?since=42");
});

test("the feed is opened again after a wait that doubles, up to half a minute", () => {
  assert.deepEqual([0, 1, 2, 3, 5, 6, 7, 40].map(retryAfter), [1_000, 1_000, 2_000, 4_000, 16_000, 30_000, 30_000, 30_000]);
});

test("a feed is opened again after an end another try may get past", () => {
  assert.deepEqual(
    (["connection.lost", "connection.refused", "connection.no_answer", "connection.other_protocol", "connection.server_too_old"] as const).map(retries),
    [true, true, true, false, false],
  );
});

test("a change is heard once, and only from its own workspace", () => {
  const change = (workspace: string, seq: number): Incoming => ({ id: 0, type: "workspace", workspace, seq, change: { kind: "chat_changed", chat: "c-1" } });
  const welcome: Incoming = { id: 1, type: "welcome", app: { name: "weacon-server", version: "0.0.0", commit: "0" } };

  assert.equal(heardAfter(change("w-1", 8), "w-1", 7), 8);
  assert.equal(heardAfter(change("w-1", 7), "w-1", 7), undefined);
  assert.equal(heardAfter(change("w-1", 3), "w-1", 7), undefined);
  assert.equal(heardAfter(change("w-2", 9), "w-1", 7), undefined);
  assert.equal(heardAfter(welcome, "w-1", 7), 7);
});

test("a feed that stayed restarts the backoff, and one welcomed and closed at once doesn't", () => {
  assert.equal(failedAfter(0, false), 1);
  assert.equal(failedAfter(4, false), 5);
  assert.equal(failedAfter(4, true), 1);
});
