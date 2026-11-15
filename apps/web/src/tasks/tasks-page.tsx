import { useEffect, useState } from "react";

import type { Api } from "../api/api.ts";
import { useAsk } from "../api/ask.ts";
import { type Loading, useLoaded } from "../api/loaded.ts";
import { Problem } from "../api/problem.tsx";
import type { TaskList, TaskRecord, TaskStatus, WorkspaceId } from "../protocol.ts";
import { useBurst } from "../shell/feed-hooks.ts";
import type { WorkspaceSocket } from "../shell/workspace-socket.ts";
import { STATUSES, withTask } from "./tasks.ts";
import "./tasks.css";

/** The workspace's tasks in their order, kept fresh by its feed, to add to, retitle and move along. */
export function TasksPage({ api, workspace, socket }: { api: Api; workspace: WorkspaceId; socket: WorkspaceSocket }) {
  const tasks = useLoaded(() => api.tasks(workspace), workspace);
  const reload = useBurst(() => void tasks.load());
  useEffect(
    () =>
      socket.subscribe((message) => {
        if (message.type === "workspace" && message.change.kind === "task_changed") {
          reload();
        }
      }),
    [socket],
  );
  const list = tasks.value?.tasks;
  return (
    <div className="page tasks">
      {tasks.loading && <p className="muted">Loading</p>}
      {list?.length === 0 && <p className="muted">No tasks yet.</p>}
      {list !== undefined && list.length > 0 && (
        <ul aria-label="Tasks">
          {list.map((task) => (
            <TaskRow key={task.id} api={api} workspace={workspace} task={task} tasks={tasks} />
          ))}
        </ul>
      )}
      {tasks.problem !== undefined && <Problem text={tasks.problem} retry={() => void tasks.load()} />}
      {list !== undefined && <NewTask api={api} workspace={workspace} tasks={tasks} />}
    </div>
  );
}

type Row = { api: Api; workspace: WorkspaceId; task: TaskRecord; tasks: Loading<TaskList> };

/**
 * One task, its status to change, and its title to edit. An edit holds the version it began from,
 * so a change made meanwhile is reported, and the edit stays to be saved again over it.
 */
function TaskRow({ api, workspace, task, tasks }: Row) {
  // Without a version, the edit goes over the task as it is now, after a change it was told of.
  const [draft, setDraft] = useState<{ title: string; version?: number }>();
  const asking = useAsk();
  const saved = (record: TaskRecord) => tasks.change((list) => withTask(list, record));
  const stale = (code: string) => {
    if (code === "task.stale") {
      void tasks.load();
    }
  };
  const status = (next: TaskStatus) => void asking.ask(() => api.changeTask(workspace, task.id, task.version, { kind: "status", status: next }), saved, stale);
  const save = (title: string, version: number) =>
    void asking.ask(
      () => api.changeTask(workspace, task.id, version, { kind: "title", title: title.trim() }),
      (record) => {
        setDraft(undefined);
        saved(record);
      },
      (code) => {
        stale(code);
        setDraft((held) => (code === "task.stale" && held !== undefined ? { title: held.title } : held));
      },
    );
  return (
    <li>
      <select aria-label={`Status of ${task.title}`} value={task.status} disabled={asking.busy} onChange={(event) => status(event.target.value as TaskStatus)}>
        {STATUSES.map(([value, text]) => (
          <option key={value} value={value}>
            {text}
          </option>
        ))}
      </select>
      {draft === undefined ? (
        <button
          type="button"
          className={task.status === "done" ? "title done" : "title"}
          aria-label={`Edit ${task.title}`}
          onClick={() => {
            asking.clear();
            setDraft({ title: task.title, version: task.version });
          }}
        >
          {task.title}
        </button>
      ) : (
        <form
          onSubmit={(event) => {
            event.preventDefault();
            save(draft.title, draft.version ?? task.version);
          }}
        >
          <input aria-label="Title" required autoFocus value={draft.title} onChange={(event) => setDraft({ ...draft, title: event.target.value })} />
          <button type="submit" disabled={asking.busy}>
            Save
          </button>
          <button
            type="button"
            onClick={() => {
              asking.clear();
              setDraft(undefined);
            }}
          >
            Cancel
          </button>
        </form>
      )}
      {asking.problem !== undefined && <p role="alert">{asking.problem}</p>}
      {draft !== undefined && draft.version === undefined && <p className="muted">It now reads “{task.title}”. Save to put yours in its place.</p>}
    </li>
  );
}

function NewTask({ api, workspace, tasks }: { api: Api; workspace: WorkspaceId; tasks: Loading<TaskList> }) {
  const [title, setTitle] = useState("");
  const { busy, problem, ask } = useAsk();
  return (
    <form
      className="new-task"
      onSubmit={(event) => {
        event.preventDefault();
        void ask(
          () => api.createTask(workspace, title.trim()),
          (made) => {
            setTitle("");
            tasks.change((list) => withTask(list, made));
          },
        );
      }}
    >
      <input aria-label="New task" placeholder="New task" required value={title} onChange={(event) => setTitle(event.target.value)} />
      <button type="submit" disabled={busy}>
        Add
      </button>
      {problem !== undefined && <p role="alert">{problem}</p>}
    </form>
  );
}
