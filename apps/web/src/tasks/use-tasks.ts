import type { Api } from "../api/api.ts";
import { type Loading, answered, useLoaded } from "../api/loaded.ts";
import type { TaskList, WorkspaceId } from "../protocol.ts";
import { useReloadOn } from "../shell/feed-hooks.ts";
import type { WorkspaceSocket } from "../shell/workspace-socket.ts";

/** `workspace`'s tasks in their order, kept fresh by its feed; none are asked for while the server doesn't serve them. */
export function useTasks(api: Api, workspace: WorkspaceId, socket: WorkspaceSocket, served = true): Loading<TaskList> {
  const tasks = useLoaded(() => (served ? api.tasks(workspace) : answered({ tasks: [] })), `${workspace} ${served}`);
  useReloadOn(socket, ({ kind }) => served && kind === "task_changed", () => void tasks.load(), [served]);
  return tasks;
}
