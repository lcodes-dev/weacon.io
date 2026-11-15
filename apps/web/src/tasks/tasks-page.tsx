import { useEffect, useRef, useState } from "react";

import type { Api } from "../api/api.ts";
import { useAsk } from "../api/ask.ts";
import type { Loading } from "../api/loaded.ts";
import { Problem } from "../api/problem.tsx";
import type { Code, TaskId, TaskList, TaskRecord, TaskStatus, WorkspaceId } from "../protocol.ts";
import type { WorkspaceSocket } from "../shell/workspace-socket.ts";
import { STATUSES, withTask } from "./tasks.ts";
import { useTasks } from "./use-tasks.ts";
import "./tasks.css";

/**
 * The workspace's tasks in their order, kept fresh by its feed, to add to, retitle, move along and
 * start a chat for, with the one `shown` in view.
 */
export function TasksPage({
  api,
  workspace,
  socket,
  shown,
  startChat,
}: {
  api: Api;
  workspace: WorkspaceId;
  socket: WorkspaceSocket;
  shown?: TaskId | undefined;
  /** Opens New chat for an open task; none where chats can't be started. */
  startChat?: ((task: TaskId) => void) | undefined;
}) {
  const tasks = useTasks(api, workspace, socket);
  const list = tasks.value?.tasks;
  return (
    <div className="page tasks">
      {tasks.loading && <p className="muted">Loading</p>}
      {list?.length === 0 && <p className="muted">No tasks yet.</p>}
      {list !== undefined && list.length > 0 && (
        <ul aria-label="Tasks">
          {list.map((task) => (
            <TaskRow key={task.id} api={api} workspace={workspace} task={task} tasks={tasks} shown={task.id === shown} startChat={startChat} />
          ))}
        </ul>
      )}
      {tasks.problem !== undefined && <Problem text={tasks.problem} retry={() => void tasks.load()} />}
      {list !== undefined && <NewTask api={api} workspace={workspace} tasks={tasks} />}
    </div>
  );
}

type Row = { api: Api; workspace: WorkspaceId; task: TaskRecord; tasks: Loading<TaskList>; shown: boolean; startChat: ((task: TaskId) => void) | undefined };

/**
 * One task, its status to change, and its title to edit. An edit holds the version it began from,
 * so a change made meanwhile is reported, and the edit stays to be saved again over it.
 */
function TaskRow({ api, workspace, task, tasks, shown, startChat }: Row) {
  const row = useRef<HTMLLIElement>(null);
  useEffect(() => {
    if (shown) {
      row.current?.scrollIntoView({ block: "nearest" });
    }
  }, [shown]);
  // Without a version, the edit goes over the task as it is now, after a change it was told of.
  const [draft, setDraft] = useState<{ title: string; version?: number }>();
  const asking = useAsk();
  const saved = (record: TaskRecord) => tasks.change((list) => withTask(list, record));
  const stale = (code: Code) => {
    if (code === "task.stale") {
      void tasks.load();
    }
  };
  // A status saved here moves an open title edit on with it, so the edit isn't stale by its own row.
  const status = (next: TaskStatus) =>
    void asking.ask(
      () => api.changeTask(workspace, task.id, task.version, { kind: "status", status: next }),
      (record) => {
        setDraft((held) => (held?.version === task.version ? { ...held, version: record.version } : held));
        saved(record);
      },
      stale,
    );
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
    <li ref={row} className={shown ? "shown" : undefined} aria-current={shown || undefined}>
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
          <input aria-label="Title" required autoFocus readOnly={asking.busy} value={draft.title} onChange={(event) => setDraft({ ...draft, title: event.target.value })} />
          <button type="submit" disabled={asking.busy}>
            Save
          </button>
          <button
            type="button"
            disabled={asking.busy}
            onClick={() => {
              asking.clear();
              setDraft(undefined);
            }}
          >
            Cancel
          </button>
        </form>
      )}
      {draft === undefined && startChat !== undefined && task.status !== "done" && (
        <button type="button" aria-label={`Start a chat for ${task.title}`} onClick={() => startChat(task.id)}>
          Start a chat
        </button>
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
      <input aria-label="New task" placeholder="New task" required readOnly={busy} value={title} onChange={(event) => setTitle(event.target.value)} />
      <button type="submit" disabled={busy}>
        Add
      </button>
      {problem !== undefined && <p role="alert">{problem}</p>}
    </form>
  );
}
