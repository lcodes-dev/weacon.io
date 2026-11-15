import { useEffect, useRef, useState } from "react";

import { answered, useLoaded } from "../api/loaded.ts";
import { Problem } from "../api/problem.tsx";
import { useChatCommands } from "../chat/chat-hooks.ts";
import { Composer } from "../composer/composer.tsx";
import type { Send } from "../composer/prompt.ts";
import { later } from "../connection.ts";
import { newId } from "../ids.ts";
import { vendorName } from "../chat/label.ts";
import { current, repos } from "../machines/remote-settings.ts";
import type { AgentKind, ChatId, MachineId, MachineSettings, MachineSummary, RepoId } from "../protocol.ts";
import type { WorkspaceSocket } from "../shell/workspace-socket.ts";
import { type Readiness, Starts, missingText, signedIn, startBlocked, startProblem } from "./start.ts";



/**
 * Starts a chat on one of `machines`, the person's own that run chats: in a repository the
 * machine has for the workspace, with an agent, by its first prompt. Once the server takes it,
 * `started` hears the chat's id; until then the box keeps the text. Back from `away`, such as
 * Settings, where an agent may have been signed in, it asks the machine again.
 */
export function NewChat({
  socket,
  live,
  away,
  machines,
  started,
  openSettings,
}: {
  socket: WorkspaceSocket;
  /** Whether the feed is up and takes commands. */
  live: boolean;
  away: boolean;
  machines: MachineSummary[];
  started(chat: ChatId): void;
  /** Opens Settings, where the person signs an agent in on a machine, with `machine`'s own open. */
  openSettings(machine?: MachineId): void;
}) {
  const [picked, setPicked] = useState<MachineId>();
  const machine = machines.find(({ id }) => id === picked) ?? machines[0];
  const id = machine?.id;
  const key = `${id} ${socket.workspace}`;
  const settings = useLoaded(() => (id === undefined ? answered<MachineSettings | undefined>(undefined) : current(socket, id, later)), key);
  const listed = useLoaded(() => (id === undefined ? answered<RepoId[]>([]) : repos(socket, id, later)), key);
  const back = useRef(away);
  useEffect(() => {
    if (back.current && !away) {
      void settings.load();
      void listed.load();
    }
    back.current = away;
  }, [away]);
  const [repo, setRepo] = useState<RepoId>();
  const [agent, setAgent] = useState<AgentKind>("claude");
  const commands = useChatCommands(socket, undefined, live, () => {});
  const [starts] = useState(() => new Starts());
  const opened = useRef(started);
  opened.current = started;
  useEffect(
    () =>
      commands.lates((late) => {
        const chat = starts.end(late.request, late.settled);
        if (chat !== undefined) {
          opened.current(chat);
        }
      }),
    [commands.lates],
  );
  const shownRepos = listed.value;
  const chosenRepo = shownRepos?.find((one) => one === repo) ?? shownRepos?.[0];
  // Once the machine answers, only the agents signed in there for this workspace are offered.
  const offered = signedIn(settings.value);
  const chosenAgent = offered.find((kind) => kind === agent) ?? offered[0];
  const signIn = chosenAgent === undefined ? "signed_out" : settings.value?.sign_ins.find((one) => one.agent === chosenAgent)?.state;
  const agentName = chosenAgent === undefined ? "No agent" : vendorName(chosenAgent);
  const blocked = machine === undefined ? "Pick a machine to start a chat." : startBlocked({ live, repos: shownRepos, agent: agentName, signIn });
  const send: Send = async (text, request) => {
    if (machine === undefined || chosenRepo === undefined || chosenAgent === undefined) {
      return "refused";
    }
    const start = starts.begin(request, socket.workspace, text, { machine: machine.id, repo: chosenRepo, agent: chosenAgent }, newId);
    const sent = await commands.start(start.chatRef, start.action, request);
    const chat = starts.end(request, sent);
    if (chat !== undefined) {
      started(chat);
    }
    return sent;
  };
  return (
    <section className="new-chat" aria-label="New chat">
      <label>
        Machine
        <select value={machine?.id} onChange={(event) => setPicked(event.target.value)}>
          {machines.map((one) => (
            <option key={one.id} value={one.id}>
              {one.name}
            </option>
          ))}
        </select>
      </label>
      {shownRepos !== undefined && shownRepos.length > 0 && (
        <label>
          Repository
          <select value={chosenRepo} onChange={(event) => setRepo(event.target.value)}>
            {shownRepos.map((one) => (
              <option key={one} value={one}>
                {one}
              </option>
            ))}
          </select>
        </label>
      )}
      {shownRepos?.length === 0 && (
        <p className="muted">
          No repository is added on this machine for this workspace. Run <code>weacon repo add --workspace {socket.workspace} &lt;name&gt; &lt;folder&gt;</code> on
          it, then try again.{" "}
          <button type="button" onClick={() => void listed.load()}>
            Try again
          </button>
        </p>
      )}
      {listed.problem !== undefined && <Problem text={listed.problem} retry={() => void listed.load()} />}
      {offered.length > 0 && (
        <label>
          Agent
          <select value={chosenAgent} onChange={(event) => setAgent(offered.find((kind) => kind === event.target.value) ?? agent)}>
            {offered.map((kind) => (
              <option key={kind} value={kind}>
                {vendorName(kind)}
              </option>
            ))}
          </select>
        </label>
      )}
      {settings.value !== undefined && offered.length < 2 && (
        <p>
          {offered.length === 0 && "No agent is signed in for this workspace on this machine. "}
          <button type="button" onClick={() => openSettings(machine?.id)}>
            Sign in from Settings
          </button>
        </p>
      )}
      {settings.problem !== undefined && <Problem text={settings.problem} retry={() => void settings.load()} />}
      <Composer send={send} blocked={blocked} hidden={false} lates={commands.lates} explain={startProblem} />
    </section>
  );
}

/**
 * Why New chat can't be used, in one line, with what fixes it: Settings, to pair a machine, and
 * Check again; nothing when it can. A `problem` fetching the machines shows with Try again.
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
