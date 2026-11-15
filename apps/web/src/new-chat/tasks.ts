import type { TaskRecord } from "../protocol.ts";

/** The most tasks the task field lists at once. */
export const LISTED = 8;

/** The open tasks whose title holds `text`, whatever its case, in their order, at most `LISTED`; none for blank text. */
export function matching(tasks: readonly TaskRecord[], text: string): TaskRecord[] {
  const words = text.trim().toLocaleLowerCase();
  if (words === "") {
    return [];
  }
  return tasks.filter((task) => task.status !== "done" && task.title.toLocaleLowerCase().includes(words)).slice(0, LISTED);
}
