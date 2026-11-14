import assert from "node:assert/strict";
import { test } from "node:test";

import type { Incoming } from "../incoming.ts";
import type { Envelope } from "../protocol.ts";
import { FeedSocket } from "./workspace-socket.ts";

const change: Incoming = { id: 0, type: "workspace", workspace: "w-1", seq: 3, change: { kind: "chat_changed", chat: "c-1" } };
const ask: Envelope = { id: 2, type: "chat_query", workspace: "w-1", chat: "c-1", query: { kind: "snapshot" } };

test("a page sends only while the feed is live, through whichever connection is open", () => {
  const socket = new FeedSocket();
  const first: Envelope[] = [];
  const second: Envelope[] = [];

  assert.equal(socket.send(ask), false);
  socket.attach((envelope) => first.push(envelope));
  assert.equal(socket.send(ask), true);
  socket.attach(undefined);
  assert.equal(socket.send(ask), false);
  socket.attach((envelope) => second.push(envelope));
  assert.equal(socket.send(ask), true);

  assert.deepEqual([first, second], [[ask], [ask]]);
});

test("each subscription hears every message until it ends", () => {
  const socket = new FeedSocket();
  const heard: string[] = [];
  const handle = (message: Incoming) => heard.push(message.type);
  const first = socket.subscribe(handle);
  const second = socket.subscribe(handle);

  socket.heard(change);
  first();
  socket.heard(change);
  second();
  second();
  socket.heard(change);

  assert.deepEqual(heard, ["workspace", "workspace", "workspace"]);
});

test("a handler that ends its own subscription doesn't stop the others hearing the message", () => {
  const socket = new FeedSocket();
  const heard: string[] = [];
  const end = socket.subscribe(() => {
    heard.push("first");
    end();
  });
  socket.subscribe(() => heard.push("second"));

  socket.heard(change);
  socket.heard(change);

  assert.deepEqual(heard, ["first", "second", "second"]);
});

test("envelope ids count up from the hello's", () => {
  const socket = new FeedSocket();

  assert.deepEqual([socket.nextId(), socket.nextId(), socket.nextId()], [2, 3, 4]);
});
