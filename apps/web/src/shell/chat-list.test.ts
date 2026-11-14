import assert from "node:assert/strict";
import { test } from "node:test";

import type { ChatSummary } from "../protocol.ts";
import { newestFirst, stateNote } from "./chat-list.ts";

const chat = (id: string, updated_at: number): ChatSummary => ({ id, title: id, owner: "m-1", machine: "laptop", state: "open", updated_at });

test("chats are listed newest first, and by id when they tie", () => {
  const chats = [chat("c-1", 10), chat("c-3", 30), chat("c-2", 30), chat("c-0", 20)];

  assert.deepEqual(
    newestFirst(chats).map(({ id }) => id),
    ["c-2", "c-3", "c-0", "c-1"],
  );
  assert.equal(chats[0]?.id, "c-1");
});

test("a chat that takes no prompts says why", () => {
  assert.equal(stateNote("open"), undefined);
  assert.equal(stateNote("full"), "Full");
  assert.equal(stateNote("detached"), "Read only");
  assert.equal(stateNote("newer" as ChatSummary["state"]), undefined);
});
