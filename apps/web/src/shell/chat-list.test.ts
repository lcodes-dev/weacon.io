import assert from "node:assert/strict";
import { test } from "node:test";

import type { ChatSummary, RunState } from "../protocol.ts";
import { reached, newestFirst, rowNote, stateNote } from "./chat-list.ts";

const chat = (id: string, updated_at: number): ChatSummary => ({ id, title: id, owner: "m-1", legs: [], state: "open", updated_at });

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
  assert.equal(stateNote("newer" as ChatSummary["state"]), undefined);
});

test("a person's agents in a chat are those on machines they may call, whoever called each last, their own first", () => {
  const leg = (member: string, detached = false) => ({ member, machine_id: `mc-${member}`, machine: "laptop", agent: "claude" as const, detached });
  const shared = { ...chat("c-1", 10), legs: [leg("m-1"), leg("m-2"), leg("m-4", true)] };
  const on = (own: string[], others: string[]) => reached(own, others, shared).map((one) => [one.leg.member, one.own, one.shared]);

  assert.deepEqual(on(["mc-m-2"], []), [["m-2", true, false]]);
  // One of their own that they share is both, and comes once.
  assert.deepEqual(on(["mc-m-2"], ["mc-m-1", "mc-m-2"]), [["m-2", true, true], ["m-1", false, true]]);
  // An agent that can never run again isn't one to call.
  assert.deepEqual(on(["mc-m-3", "mc-m-4"], []), []);
  assert.deepEqual(reached(["mc-m-1"], [], undefined), []);
  assert.deepEqual(reached(["mc-m-1"], [], chat("c-2", 10)), []);
});

test("a chat's row names who started it once known, and why it takes no messages", () => {
  const chat: ChatSummary = { id: "c-1", title: "Plan", owner: "m-1", legs: [], state: "full", updated_at: 1 };
  const names = new Map([["m-1", "ada"]]);

  assert.equal(rowNote(chat, names), "ada · Full");
  assert.equal(rowNote({ ...chat, state: "open" }, names), "ada");
  assert.equal(rowNote(chat, new Map()), "Full");
});

test("a chat's row says what its latest run is doing, in plain words, where it is known", () => {
  const chat: ChatSummary = { id: "c-1", title: "Plan", owner: "m-1", legs: [], state: "open", updated_at: 1 };
  const names = new Map([["m-1", "ada"]]);
  const cases: [RunState | undefined, string][] = [
    ["starting", "ada · Starting"],
    ["running", "ada · Running"],
    ["waiting", "ada · Waiting for a person"],
    ["idle", "ada · Idle"],
    ["sleeping", "ada · Idle"],
    ["ended", "ada · Ended"],
    ["paused" as RunState, "ada"],
    ["constructor" as RunState, "ada"],
    ["__proto__" as RunState, "ada"],
    [undefined, "ada"],
  ];

  for (const [run, expected] of cases) {
    assert.equal(rowNote(run === undefined ? chat : { ...chat, run }, names), expected, String(run));
  }
  assert.equal(rowNote({ ...chat, run: "running", state: "full" }, names), "ada · Running · Full");
});
