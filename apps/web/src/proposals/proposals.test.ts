import assert from "node:assert/strict";
import { test } from "node:test";

import type { ProposalRecord, TaskEditBody, TaskRecord } from "../protocol.ts";
import { aboutText, askText, conflictText, pinChanges, staleText, stateText, taskEditText } from "./proposals.ts";

const AUTHOR = { kind: "agent", owner: "m-1" } as const;
const TASKS: TaskRecord[] = [
  { id: "t-1", title: "Buy flour", status: "todo", order: "a", version: 2 },
  { id: "t-2", title: "Bake", status: "todo", order: "b", version: 1 },
];
const content = { title: "Recipes", body: "" };

test("a proposal says what it is about, by the title it would have", () => {
  const cases: [Pick<ProposalRecord, "about" | "change">, string][] = [
    [{ about: { kind: "doc", doc: "d-1" }, change: { kind: "doc_edit", doc: "d-1", base: 1, content } }, "Edit of the doc “Recipes”"],
    [{ about: { kind: "doc", doc: "d-1" } }, "Edit of a doc"],
    [{ about: { kind: "doc", doc: "d-1" }, change: { kind: "needs_update", doc: "d-1", base: 1, anchors: [] } }, "Code a doc links to changed"],
    [{ about: { kind: "task", task: "t-1" } }, "Edit of the task “Buy flour”"],
    [{ about: { kind: "task", task: "t-9" } }, "Edit of a task"],
    [{ about: { kind: "new_doc", doc: "d-2" }, change: { kind: "new_doc", content } }, "New doc “Recipes”"],
    [{ about: { kind: "new_doc", doc: "d-2" } }, "New doc"],
    [{ about: { kind: "new_task", task: "t-3" }, change: { kind: "new_task", title: "Sift" } }, "New task “Sift”"],
    [{ about: { kind: "new_task", task: "t-2" } }, "Bake"],
    [{ about: { kind: "new_task", task: "t-9" } }, "New task"],
    [{ about: { kind: "elsewhere" } as unknown as ProposalRecord["about"] }, "A change this app can't show"],
  ];

  for (const [part, expected] of cases) {
    const proposal = { id: "p-1", author: AUTHOR, at: 1, state: "open", ...part } as ProposalRecord;
    assert.equal(aboutText(proposal, TASKS), expected, JSON.stringify(part));
  }
});

test("each task edit reads as a line", () => {
  const cases: [TaskEditBody, string][] = [
    [{ kind: "title", title: "Buy rye flour" }, "Retitle it “Buy rye flour”"],
    [{ kind: "status", status: "todo" }, "Mark it to do"],
    [{ kind: "status", status: "in_progress" }, "Mark it in progress"],
    [{ kind: "status", status: "done" }, "Mark it done"],
    [{ kind: "move" }, "Move it last"],
    [{ kind: "move", before: "t-2" }, "Move it before “Bake”"],
    [{ kind: "move", before: "t-9" }, "Move it"],
    [{ kind: "split" } as unknown as TaskEditBody, "An edit this app can't show"],
  ];

  for (const [edit, expected] of cases) {
    assert.equal(taskEditText(edit, TASKS), expected, JSON.stringify(edit));
  }
});

test("a decided proposal says how it ended and who decided, and a conflict what to do", () => {
  const names = new Map([["m-2", "bo"]]);
  const at = (state: object) => ({ id: "p-1", about: { kind: "doc", doc: "d-1" }, author: AUTHOR, at: 1, ...state }) as ProposalRecord;

  assert.equal(stateText(at({ state: "open" }), names), undefined);
  assert.equal(stateText(at({ state: "accepted", by: "m-2", version: 3 }), names), "Accepted by bo");
  assert.equal(stateText(at({ state: "rejected", by: "m-9" }), names), "Rejected by a member");
  assert.equal(stateText(at({ state: "superseded" }), names), "Replaced by a newer edit");
  assert.equal(stateText(at({ state: "withdrawn" }), names), "Withdrawn");
  assert.equal(stateText(at({ state: "deferred" }), names), "Closed");
  assert.match(conflictText({ outcome: "doc_conflict", hunks: [], title: true, anchors: false }) ?? "", /^The doc changed/);
  assert.match(conflictText({ outcome: "task_conflict", edits: [] }) ?? "", /^The task changed/);
  assert.equal(conflictText({ outcome: "accepted", version: 2 }), undefined);
  assert.equal(conflictText({ outcome: "rejected" }), undefined);
});

test("a doc edit's pins say which links to code it adds and which it drops", () => {
  const commit = "a".repeat(40);
  const pins = (...pins: string[]) => ({ commit, pins });

  assert.deepEqual(pinChanges(pins("sym:a#0000000000000000", "sym:b#1111111111111111"), pins("sym:b#1111111111111111", "sym:c#2222222222222222")), {
    added: ["sym:c#2222222222222222"],
    removed: ["sym:a#0000000000000000"],
  });
  assert.deepEqual(pinChanges(undefined, pins("flow:x#3333333333333333")), { added: ["flow:x#3333333333333333"], removed: [] });
  assert.deepEqual(pinChanges(pins("sym:a#0000000000000000"), undefined), { added: [], removed: ["sym:a#0000000000000000"] });
  assert.deepEqual(pinChanges(undefined, undefined), { added: [], removed: [] });
});

test("each anchor a move changed says what it was and is, and the ask names the doc", () => {
  const pin = "sym:a::go#0123456789abcdef";
  const anchors = [
    { pin, change: { kind: "changed", was: "fn a::go  a.rs:1", now: "fn a::go  a.rs:4" } },
    { pin, change: { kind: "gone", was: "fn a::go  a.rs:1" } },
    { pin, change: { kind: "moved" } as unknown as { kind: "gone"; was: string } },
  ] as const;

  assert.deepEqual(anchors.map(staleText), [
    "sym:a::go changed. It was “fn a::go  a.rs:1”, and is now “fn a::go  a.rs:4”.",
    "sym:a::go is gone. It was “fn a::go  a.rs:1”.",
    "sym:a::go changed.",
  ]);
  assert.equal(
    askText({ kind: "needs_update", doc: "d-1", base: 2, anchors: [] }),
    "To update it, ask your agent in a chat: “Propose an update to the doc d-1 where it describes the code that changed.”",
  );
});
