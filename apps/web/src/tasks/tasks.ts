import type { TaskList, TaskRecord, TaskStatus } from "../protocol.ts";

/** Each status a person can pick, in the order a task goes through them. */
export const STATUSES: ReadonlyArray<[TaskStatus, string]> = [
  ["todo", "To do"],
  ["in_progress", "In progress"],
  ["done", "Done"],
];

/** `list` with `task` in it, in place of its older version, in their order; an older answer changes nothing. */
export function withTask(list: TaskList | undefined, task: TaskRecord): TaskList {
  const others = (list?.tasks ?? []).filter(({ id }) => id !== task.id);
  const kept = list?.tasks.find(({ id }) => id === task.id);
  const newest = kept !== undefined && kept.version > task.version ? kept : task;
  const tasks = [...others, newest].sort((a, b) => (a.order === b.order ? 0 : a.order < b.order ? -1 : 1));
  return { tasks };
}
