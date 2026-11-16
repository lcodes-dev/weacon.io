import type { Api } from "../api/api.ts";
import { type Loading, useLoaded } from "../api/loaded.ts";
import type { TaskList, WorkspaceId } from "../protocol.ts";
import { useReloadOn } from "../shell/feed-hooks.ts";
import type { WorkspaceSocket } from "../shell/workspace-socket.ts";

/** `workspace`'s tasks in their order, kept fresh by its feed. */
export function useTasks(api: Api, workspace: WorkspaceId, socket: WorkspaceSocket): Loading<TaskList> {
  const tasks = useLoaded(() => api.tasks(workspace), workspace);
  useReloadOn(socket, ({ kind }) => kind === "task_changed", () => void tasks.load());
  return tasks;
}
