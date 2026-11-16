import { type ReactNode, useEffect, useState } from "react";

import type { ChatRef } from "../chat-ref.ts";
import { Composer } from "../composer/composer.tsx";
import { agentItem } from "../composer/mentionables.ts";
import { type Send, blocked, notingPlain } from "../composer/prompt.ts";
import { MyAgentFields, picked, useMyAgent } from "../new-chat/my-agent.tsx";
import type { Callable, Choice } from "../new-chat/start.ts";
import type { MachineId, MemberId } from "../protocol.ts";
import type { Reached } from "../shell/chat-list.ts";
import { ChatChanges } from "../diff/chat-changes.tsx";
import { useChanges } from "../diff/use-changes.ts";
import { ReviewBar } from "../worktree/review-bar.tsx";
import { useWorktree } from "../worktree/use-worktree.ts";
import { type WorkspaceSocket, useOffers } from "../shell/workspace-socket.ts";
import { useChatAccess } from "./access.ts";
import { calling, named, working } from "./chat.ts";
import { vendorName } from "./label.ts";
import { useChatCommands, useLiveChat } from "./chat-hooks.ts";
import { landing } from "./landing.ts";
import type { Names } from "../members/names.ts";
import { lines } from "./lines.ts";
import { StopButton } from "./stop-button.tsx";
import { TranscriptView } from "./transcript-view.tsx";
import "./chat.css";

/**
 * The chat at `chatRef` over its workspace's `link`: its transcript and the message box, for every
 * member, kept in place while `away` under another page such as Settings. `me` calls any of
 * `agents`, those the chat has on machines they may call, and for the first of them also gets its
 * changes, Stop and its permission prompts' answers, where the server takes commands. With none
 * of their own there, they pick theirs from `machines`, or read what is `missing`.
 */
export function ChatScreen({
  link,
  live,
  chatRef,
  me,
  agents,
  machines,
  missing,
  openSettings,
  away,
  title,
  names,
  at,
}: {
  link: WorkspaceSocket;
  /** Whether the link's connection is up. */
  live: boolean;
  chatRef: ChatRef | undefined;
  me: MemberId | undefined;
  agents: Reached[];
  /** The machines the reader may call an agent on. */
  machines: Callable[];
  missing: ReactNode;
  openSettings(machine?: MachineId): void;
  away: boolean;
  /** The chat's title, a pull request's to start with. */
  title?: string | undefined;
  /** The workspace's members' names, for its people and its agents' owners. */
  names?: Names;
  /** The message in the chat's log to open it at, rather than at its latest. */
  at?: number | undefined;
}) {
  const [shown, setShown] = useState<"run" | "changes">("run");
  const access = useChatAccess(link);
  const reading = useLiveChat(link, chatRef, live, access.read);
  const commands = useChatCommands(link, chatRef, live, reading.answered);
  const mine = agents.length > 0;
  // The agent whose changes show: the one picked, while it is still one to call, else the first.
  const [picked, setPicked] = useState<MachineId>();
  const reviewed = agents.find(({ leg }) => leg.machine_id === picked) ?? agents[0];
  const diffs = useOffers(link, "diff.view");
  const { chat, state } = reading.sync;
  // A message named while the changes show brings the transcript back.
  useEffect(() => {
    if (at !== undefined) {
      setShown("run");
    }
  }, [at]);
  const landed = landing(chat, at);
  const subscribed = state.kind === "live";
  // Each window that comes back moves `first` on, and asks for the next until the message is in.
  useEffect(() => {
    if (landed.kind === "older" && subscribed) {
      reading.more();
    }
  }, [landed.kind, chat?.first, subscribed]);
  // A run that names the reader is theirs to stop, and to answer where its machine is theirs,
  // in any chat and before the list shows their agent there.
  const answers = access.send && (mine || (chat !== undefined && named(chat, me)));
  const stop = answers && chat !== undefined && working(chat, me) ? commands.stop : undefined;
  const why = blocked(live, chatRef, access.send);
  const askable = mine && chatRef !== undefined && live && diffs;
  return (
    <div className="chat-screen">
      {(stop !== undefined || askable || shown === "changes") && (
        <div className="toolbar">
          {shown === "run" ? (
            <>
              {stop !== undefined && <StopButton stopping={stop.stopping} problem={stop.problem} onStop={stop.stop} />}
              {askable && (
                <button type="button" onClick={() => setShown("changes")}>
                  Changes
                </button>
              )}
            </>
          ) : (
            <>
              <button type="button" onClick={() => setShown("run")}>
                Back
              </button>
              {agents.length > 1 && (
                <label>
                  Changes of
                  <select value={reviewed?.leg.machine_id} onChange={(event) => setPicked(event.target.value)}>
                    {agents.map(({ leg, shared }) => (
                      <option key={leg.machine_id} value={leg.machine_id}>
                        {shared ? agentItem(leg.machine_id, leg.agent, leg.machine).label : `${vendorName(leg.agent)} · ${leg.machine}`}
                      </option>
                    ))}
                  </select>
                </label>
              )}
            </>
          )}
        </div>
      )}
      <div className="panes">
        <div className={shown === "run" ? "pane" : "pane away"}>
          <TranscriptView
            lines={chat === undefined ? [] : lines(chat, names)}
            older={chat !== undefined && state.kind === "live" && chat.first > 1 ? reading.more : undefined}
            answering={answers ? commands.answering : undefined}
            me={me}
            // A chat is on its way while the connection is made, then only from a server that serves chats.
            loading={chatRef !== undefined && chat === undefined && state.kind !== "failed" && (!live || access.read)}
            retry={state.kind === "failed" ? reading.open : undefined}
            away={away || shown !== "run"}
            landed={landed.kind === "line" ? landed.key : undefined}
          />
        </div>
        {reviewed !== undefined && chatRef !== undefined && (
          // Its comments, its worktree's answers and a pull request's draft are one agent's alone.
          <AgentChanges
            key={reviewed.leg.machine_id}
            link={link}
            chatRef={chatRef}
            agent={reviewed}
            live={live}
            shown={shown === "changes"}
            send={access.send ? commands.send() : undefined}
            blocked={why}
            title={title ?? ""}
          />
        )}
      </div>
      <Calls
        link={link}
        agents={agents}
        machines={machines}
        missing={missing}
        openSettings={openSettings}
        away={away}
        send={commands.send}
        blocked={why}
        hidden={shown !== "run"}
        calling={calling(chat, me)}
      />
    </div>
  );
}

// The message box, which calls any of the chat's `agents` the reader may call. One with none of
// their own there may pick theirs from `machines`: until the chat has it, each call of it names
// the pick. The box stays as the fields open and as the chat's list catches up, so what is typed
// does.
function Calls({
  link,
  agents,
  machines,
  missing,
  openSettings,
  away,
  send,
  blocked: why,
  hidden,
  calling,
}: {
  link: WorkspaceSocket;
  agents: Reached[];
  machines: Callable[];
  missing: ReactNode;
  openSettings(machine?: MachineId): void;
  away: boolean;
  send(first?: Choice): Send;
  blocked: string | undefined;
  hidden: boolean;
  /** Whether the reader's last message here called an agent. */
  calling: boolean;
}) {
  const [adding, setAdding] = useState(false);
  // After a plain message the box stays plain: the next call is one the person writes.
  const [plain, setPlain] = useState(false);
  // An agent the chat has is called, not picked again; once it has the one picked, the fields go.
  useEffect(() => setAdding(false), [agents.length]);
  const free = machines.filter(({ id }) => !agents.some(({ leg }) => leg.machine_id === id));
  // Nothing is asked of a machine until the person asks for their agent.
  const pick = useMyAgent(link, adding ? free : [], away);
  const fields = free.length > 0 ? <MyAgentFields pick={pick} workspace={link.workspace} openSettings={openSettings} /> : missing;
  const theirs = agents.map(({ leg, shared }) => agentItem(leg.machine_id, leg.agent, shared ? leg.machine : undefined));
  return (
    <>
      {!hidden &&
        !agents.some(({ own }) => own) &&
        (adding ? (
          fields
        ) : (
          <button type="button" className="add-agent" onClick={() => {
              setAdding(true);
              setPlain(false);
            }}>
            Add your agent
          </button>
        ))}
      <Composer
        send={notingPlain(send(pick.choice), () => setPlain(true))}
        blocked={why}
        hidden={hidden}
        agents={[...picked(pick), ...theirs]}
        calling={adding ? !plain : calling}
      />
    </>
  );
}

// The changes the chat's `agent` made, with its worktree's bar where the worktree is the reader's
// to change; `send` takes a review of them to that agent.
function AgentChanges({
  link,
  chatRef,
  agent,
  live,
  shown,
  send,
  blocked: why,
  title,
}: {
  link: WorkspaceSocket;
  chatRef: ChatRef;
  agent: Reached;
  live: boolean;
  shown: boolean;
  send: Send | undefined;
  blocked: string | undefined;
  title: string;
}) {
  const machine = agent.leg.machine_id;
  const changes = useChanges(link, chatRef, machine, live, shown);
  const worktree = useWorktree(link, chatRef, machine, live, shown);
  // The worktree of an agent on a machine someone else shares is its owner's to change.
  const bar = worktree.usable && agent.own && <ReviewBar bar={worktree} title={title} />;
  const none: Send = () => Promise.resolve("refused");
  return <ChatChanges changes={changes.changes} hidden={!shown} machine={send && machine} send={send ?? none} blocked={why} bar={bar} />;
}
