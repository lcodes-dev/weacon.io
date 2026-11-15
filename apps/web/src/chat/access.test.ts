import assert from "node:assert/strict";
import { test } from "node:test";

import type { WebCapability } from "../connection.ts";
import { chatAccess } from "./access.ts";

const offering =
  (...offered: WebCapability[]) =>
  (capability: WebCapability) =>
    offered.includes(capability);

test("a server's chats are read and sent to only as far as it offers", () => {
  assert.deepEqual(chatAccess(offering()), { read: false, send: false });
  assert.deepEqual(chatAccess(offering("workspace.chats")), { read: true, send: false });
  assert.deepEqual(chatAccess(offering("workspace.chats", "workspace.chat_commands")), { read: true, send: true });
  assert.deepEqual(chatAccess(offering("workspace.chat_commands", "diff.view")), { read: false, send: true });
});
