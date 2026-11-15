import assert from "node:assert/strict";
import { test } from "node:test";

import type { ChatEntry, RunEvent } from "../protocol.ts";
import { commit, fromSnapshot, older, stream } from "./chat.ts";
import { lines } from "./lines.ts";

const ran = (seq: number, run: string, event: RunEvent): ChatEntry => ({ seq, event: { kind: "run", update: { run, event } } });
const said = (seq: number, text: string): ChatEntry => ({ seq, event: { kind: "prompt", request: `q-${seq}`, text } });
const agent = { kind: "claude" as const, owner: { kind: "member" as const, member: "m-1" }, machine: "laptop" };

function chatOf(events: ChatEntry[], seq: number) {
  return fromSnapshot({ seq, runs: new Map([["r-1", { state: "waiting", agent }]]), waiting: [4], events });
}

test("a chat's lines are its prompts and its turns under their labels, with prompts in place", () => {
  const chat = chatOf(
    [
      said(2, "Fix it"),
      ran(3, "r-1", { event: "text", text: "Looking." }),
      ran(4, "r-1", { event: "permission_asked", run: "r-1", prompt: 1, tool: "Bash", input: null }),
    ],
    4,
  );
  const writing = stream(chat, "r-1", { event: "text_delta", text: "Run" });

  assert.deepEqual(
    lines(writing).map((line) => [line.key, line.kind, line.kind === "row" ? line.writing : line.kind === "agent" ? line.label : ""]),
    [
      ["p2", "prompt", ""],
      ["t2-r-1", "agent", "Claude Code · laptop"],
      ["t2-r-1.0", "row", false],
      ["t2-r-1.a1", "ask", ""],
      ["t2-r-1.1", "row", true],
    ],
  );
  assert.deepEqual(lines(chat).at(1), { key: "t2-r-1", kind: "agent", label: "Claude Code · laptop" });
  assert.deepEqual(lines(chat, new Map([["m-1", "ada"]])).at(1), { key: "t2-r-1", kind: "agent", label: "ada's Claude Code · laptop" });
  assert.deepEqual(lines(chat).at(-1), { key: "t2-r-1.a1", kind: "ask", run: "r-1", ask: { prompt: 1, tool: "Bash", at: 1, state: "open" } });
});

test("lines keep their keys as older events load before them and as text commits", () => {
  const chat = chatOf([said(3, "Hi")], 3);
  const streaming = stream(chat, "r-1", { event: "text_delta", text: "Hel" });
  const committed = commit(streaming, ran(4, "r-1", { event: "text", text: "Hello" }));
  assert.ok(committed !== undefined);
  const widened = older(committed, [
    ran(1, "r-1", { event: "text", text: "Sure." }),
    ran(2, "r-1", { event: "finished", outcome: { state: "completed" } }),
  ]);
  const keys = (shown: typeof chat) => lines(shown).map((line) => line.key);

  assert.deepEqual(keys(streaming), ["p3", "t3-r-1", "t3-r-1.0"]);
  assert.deepEqual(keys(committed), keys(streaming));
  assert.deepEqual(keys(widened), ["t0-r-1", "t0-r-1.0", "t0-r-1.1", "p3", "t3-r-1", "t3-r-1.0"]);
});

test("a command its machine refused shows why, in place, and an unknown reason in plain words", () => {
  const refused = (seq: number, code: string): ChatEntry => ({ seq, event: { kind: "receipt", receipt: { request: `q-${seq}`, state: "failed", code } } });
  const chat = chatOf([said(2, "Fix it"), refused(3, "chat.busy"), refused(4, "chat.newer_reason")], 4);

  assert.deepEqual(lines(chat), [
    { key: "p2", kind: "prompt", text: "Fix it" },
    { key: "f3", kind: "refused", text: "The agent is busy with this chat. Try again once it's done." },
    { key: "f4", kind: "refused", text: "The machine couldn't do this." },
  ]);
  const done: ChatEntry = { seq: 3, event: { kind: "receipt", receipt: { request: "q-2", state: "completed" } } };
  assert.equal(lines(chatOf([said(2, "Fix it"), done], 3)).length, 1);
});
