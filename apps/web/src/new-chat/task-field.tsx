import { useState } from "react";

import type { TaskId, TaskRecord } from "../protocol.ts";
import { matching } from "./tasks.ts";

/** The task a new chat is for, none to begin with: found by typing part of an open task's title. */
export function TaskField({ tasks, chosen, choose }: { tasks: readonly TaskRecord[]; chosen: TaskRecord | undefined; choose(task: TaskId | undefined): void }) {
  const [text, setText] = useState("");
  if (chosen !== undefined) {
    return (
      <p className="task-field">
        Task: <span className="title">{chosen.title}</span>{" "}
        <button type="button" onClick={() => choose(undefined)}>
          Remove
        </button>
      </p>
    );
  }
  const found = matching(tasks, text);
  return (
    <div className="task-field">
      <label>
        Task (optional)
        <input type="search" placeholder="Search open tasks" value={text} onChange={(event) => setText(event.target.value)} />
      </label>
      {found.length > 0 && (
        <ul aria-label="Matching tasks">
          {found.map((task) => (
            <li key={task.id}>
              <button
                type="button"
                onClick={() => {
                  setText("");
                  choose(task.id);
                }}
              >
                {task.title}
              </button>
            </li>
          ))}
        </ul>
      )}
      {text.trim() !== "" && found.length === 0 && <p className="muted">No open task matches.</p>}
    </div>
  );
}
