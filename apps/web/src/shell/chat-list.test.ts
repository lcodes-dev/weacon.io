import assert from "node:assert/strict";
import { test } from "node:test";

import type { ChatSummary, RunState } from "../protocol.ts";
import { newestFirst, owns, rowNote, stateNote } from "./chat-list.ts";

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

test("only the chat's owner owns it, and a person whose member isn't known owns none", () => {
  const theirs = chat("c-1", 10);
  assert.equal(owns(theirs.owner, theirs), true);
  assert.equal(owns("m-other", theirs), false);
  assert.equal(owns(undefined, theirs), false);
  assert.equal(owns(theirs.owner, undefined), false);
});

test("a chat's row names its owner once known, its machine and why it takes no prompts", () => {
  const chat = { id: "c-1", title: "Plan", owner: "m-1", machine: "laptop", state: "full", updated_at: 1 } as const;
  const names = new Map([["m-1", "ada"]]);

  assert.equal(rowNote(chat, names), "ada · laptop · Full");
  assert.equal(rowNote({ ...chat, state: "open" }, names), "ada · laptop");
  assert.equal(rowNote(chat, new Map()), "laptop · Full");
});

test("a chat's row says what its latest run is doing, in plain words, where it is known", () => {
  const chat = { id: "c-1", title: "Plan", owner: "m-1", machine: "laptop", state: "open", updated_at: 1 } as const;
  const names = new Map([["m-1", "ada"]]);
  const cases: [RunState | undefined, string][] = [
    ["starting", "ada · laptop · Starting"],
    ["running", "ada · laptop · Running"],
    ["waiting", "ada · laptop · Waiting for a person"],
    ["idle", "ada · laptop · Idle"],
    ["sleeping", "ada · laptop · Idle"],
    ["ended", "ada · laptop · Ended"],
    ["paused" as RunState, "ada · laptop"],
    ["constructor" as RunState, "ada · laptop"],
    ["__proto__" as RunState, "ada · laptop"],
    [undefined, "ada · laptop"],
  ];

  for (const [run, expected] of cases) {
    assert.equal(rowNote(run === undefined ? chat : { ...chat, run }, names), expected, String(run));
  }
  assert.equal(rowNote({ ...chat, run: "running", state: "full" }, names), "ada · laptop · Running · Full");
});
