import assert from "node:assert/strict";
import { test } from "node:test";

import type { TaskRecord } from "../protocol.ts";
import { withTask } from "./tasks.ts";

const task = (id: string, order: string, version = 1): TaskRecord => ({ id, title: id, status: "todo", order, version });

test("a saved task takes its place in the list by its order, unless the list holds a newer version", () => {
  const list = { tasks: [task("a", "a0"), task("b", "b0", 3)] };

  assert.deepEqual(withTask(list, task("c", "a5")).tasks.map(({ id }) => id), ["a", "c", "b"]);
  assert.deepEqual(withTask(list, task("a", "c0", 2)).tasks.map(({ id }) => id), ["b", "a"]);
  assert.deepEqual(withTask(list, task("b", "a0", 2)).tasks, list.tasks);
  assert.deepEqual(withTask(undefined, task("a", "a0")).tasks, [task("a", "a0")]);
});
