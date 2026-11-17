import assert from "node:assert/strict";
import { test } from "node:test";

import type { TaskRecord, TaskStatus } from "../protocol.ts";
import { LISTED, matching } from "./tasks.ts";

const task = (id: string, title: string, status: TaskStatus = "todo"): TaskRecord => ({ id, title, status, order: id, version: 1 });
const ids = (tasks: TaskRecord[]) => tasks.map(({ id }) => id);

test("the task field finds open tasks by any part of their title, whatever its case, in their order", () => {
  const tasks = [task("t-1", "Speed up the build"), task("t-2", "Fix the BUILD cache", "in_progress"), task("t-3", "Old build", "done"), task("t-4", "Write docs")];

  assert.deepEqual(ids(matching(tasks, " build ")), ["t-1", "t-2"]);
  assert.deepEqual(ids(matching(tasks, "Docs")), ["t-4"]);
  assert.deepEqual(matching(tasks, "  "), []);
  assert.deepEqual(matching(tasks, "release"), []);
});

test("the task field lists a fixed number of tasks at most", () => {
  const many = Array.from({ length: LISTED + 3 }, (_, n) => task(`t-${n}`, `Task ${n}`));

  assert.equal(matching(many, "task").length, LISTED);
  assert.equal(matching(many, "task")[0]?.id, "t-0");
});
