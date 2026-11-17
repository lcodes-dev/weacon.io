import type { TaskId, TaskRecord, WorkspaceId } from "../protocol.ts";
import { type Place, followsHere, searchOf } from "./place.ts";

/** The task a chat was made for, by its title as it reads now, linked to it among the tasks; nothing until it is listed. */
export function ChatTaskLink({ workspace, task, tasks, go }: { workspace: WorkspaceId; task: TaskId; tasks: readonly TaskRecord[] | undefined; go(place: Place): void }) {
  const found = tasks?.find(({ id }) => id === task);
  if (found === undefined) {
    return null;
  }
  const place: Place = { view: "tasks", task };
  return (
    <p className="chat-task">
      Task:{" "}
      <a
        href={searchOf(workspace, place)}
        onClick={(event) => {
          if (followsHere(event)) {
            event.preventDefault();
            go(place);
          }
        }}
      >
        {found.title}
      </a>
    </p>
  );
}
