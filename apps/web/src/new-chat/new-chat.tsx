import { type ReactNode, useState } from "react";

import { Problem } from "../api/problem.tsx";
import { useChatCommands } from "../chat/chat-hooks.ts";
import { Composer } from "../composer/composer.tsx";
import { agentItem } from "../composer/mentionables.ts";
import type { Send } from "../composer/prompt.ts";
import { newId } from "../ids.ts";
import type { ChatId, MachineId, TaskId, TaskRecord } from "../protocol.ts";
import type { WorkspaceSocket } from "../shell/workspace-socket.ts";
import { MyAgentFields, useMyAgent } from "./my-agent.tsx";
import { type Callable, type Readiness, missingText, startProblem } from "./start.ts";
import { TaskField } from "./task-field.tsx";

/**
 * Starts a chat by its first message, for one of `tasks` if the person picks one; `task` is
 * picked to begin with. With one of `machines`, their own or one shared with the workspace, they also pick
 * their agent in the chat, which a message that mentions it calls; `missing` says what calling
 * one lacks when there is none. Once the server takes the message, `started` hears the chat's
 * id; until then the box keeps the text.
 */
export function NewChat({
  socket,
  live,
  away,
  machines,
  missing,
  started,
  openSettings,
  tasks,
  task,
}: {
  socket: WorkspaceSocket;
  /** Whether the feed is up and takes commands. */
  live: boolean;
  away: boolean;
  machines: Callable[];
  missing: ReactNode;
  started(chat: ChatId): void;
  /** Opens Settings, where the person signs an agent in on a machine, with `machine`'s own open. */
  openSettings(machine?: MachineId): void;
  /** The workspace's tasks, while the server serves them. */
  tasks?: readonly TaskRecord[] | undefined;
  task?: TaskId | undefined;
}) {
  const pick = useMyAgent(socket, machines, away);
  const [taskId, setTaskId] = useState(task);
  const forTask = tasks?.find(({ id }) => id === taskId);
  const commands = useChatCommands(socket, undefined, live, () => {});
  const { choice } = pick;
  const send: Send = async (text, request, calls) => {
    // The server gives the task its title, so a task picked before the list loaded still goes;
    // one the loaded list doesn't have doesn't.
    const named = taskId !== undefined && (tasks === undefined || forTask !== undefined);
    const forIt = named && { task: { id: taskId, title: forTask?.title ?? "" } };
    const chat = newId();
    const action = calls && choice !== undefined ? { kind: "prompt" as const, text, ...choice, ...forIt } : { kind: "say" as const, text, ...forIt };
    const sent = await commands.start({ workspace: socket.workspace, chat }, action, request);
    if (sent === "accepted") {
      started(chat);
    }
    return sent;
  };
  return (
    <section className="new-chat" aria-label="New chat">
      {tasks !== undefined && <TaskField tasks={tasks} chosen={forTask} choose={setTaskId} />}
      {machines.length > 0 ? <MyAgentFields pick={pick} workspace={socket.workspace} openSettings={openSettings} /> : missing}
      <Composer
        send={send}
        blocked={live ? undefined : "Chats start once the server is connected."}
        hidden={false}
        explain={startProblem}
        agent={choice && agentItem(choice.agent)}
        calling
      />
    </section>
  );
}

/**
 * Why the person can't call an agent of their own, in one line, with what fixes it: Settings, to
 * pair a machine, and Check again; nothing when they can. A `problem` fetching the machines shows
 * with Try again.
 */
export function StartMissing({
  ready,
  problem,
  check,
  openSettings,
}: {
  ready: Readiness | undefined;
  problem: string | undefined;
  check(): void;
  openSettings(): void;
}) {
  if (ready?.kind === "ready") {
    return undefined;
  }
  if (ready === undefined && problem !== undefined) {
    return <Problem text={problem} retry={check} />;
  }
  return (
    <p className="muted">
      {ready === undefined ? "Looking for your machines." : missingText(ready.kind)}{" "}
      {ready?.kind === "unpaired" && (
        <button type="button" onClick={() => openSettings()}>
          Open Settings
        </button>
      )}{" "}
      {ready !== undefined && ready.kind !== "unserved" && (
        <button type="button" onClick={check}>
          Check again
        </button>
      )}
    </p>
  );
}
