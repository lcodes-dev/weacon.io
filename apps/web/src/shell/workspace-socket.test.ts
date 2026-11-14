import assert from "node:assert/strict";
import { test } from "node:test";

import type { Incoming } from "../incoming.ts";
import type { Envelope } from "../protocol.ts";
import { WorkspaceSocket } from "./workspace-socket.ts";

const change: Incoming = { id: 0, type: "workspace", workspace: "w-1", seq: 3, change: { kind: "chat_changed", chat: "c-1" } };
const ask: Envelope = { id: 2, type: "chat_query", workspace: "w-1", chat: "c-1", query: { kind: "snapshot" } };

test("a page sends only while the feed is live, through whichever connection is open", () => {
  const socket = new WorkspaceSocket("w-1");
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

test("a subscription hears every message until it ends", () => {
  const socket = new WorkspaceSocket("w-1");
  const heard: string[] = [];
  const end = socket.subscribe((message) => heard.push(message.type));

  socket.heard(change);
  end();
  socket.heard(change);

  assert.deepEqual(heard, ["workspace"]);
});

test("a handler that ends its own subscription doesn't stop the others hearing the message", () => {
  const socket = new WorkspaceSocket("w-1");
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
  const socket = new WorkspaceSocket("w-1");

  assert.deepEqual([socket.nextId(), socket.nextId(), socket.nextId()], [2, 3, 4]);
});

test("a socket offers what the server offered when it last welcomed the feed, across a reconnect", () => {
  const socket = new WorkspaceSocket("w-1");
  assert.equal(socket.offers("machine.settings"), false);
  socket.attach(() => {}, ["workspace.chats", "machine.settings"]);
  assert.equal(socket.offers("machine.settings"), true);
  assert.equal(socket.offers("chat.send"), false);
  socket.attach(undefined);
  assert.equal(socket.offers("machine.settings"), true);
  socket.attach(() => {}, ["workspace.chats"]);
  assert.equal(socket.offers("machine.settings"), false);
  assert.equal(socket.offers("workspace.chats"), true);
});
