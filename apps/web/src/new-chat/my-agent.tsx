import { useEffect, useRef, useState } from "react";

import { answered, useLoaded } from "../api/loaded.ts";
import { Problem } from "../api/problem.tsx";
import { vendorName } from "../chat/label.ts";
import { later } from "../connection.ts";
import { agents, repos } from "../machines/remote-settings.ts";
import type { AgentKind, MachineId, RepoId } from "../protocol.ts";
import type { WorkspaceSocket } from "../shell/workspace-socket.ts";
import { type Callable, type Choice, KINDS, agentMissing, machineLabel, offeredAgents } from "./start.ts";

/**
 * A person's pick of their agent for a chat: one of `machines`, their own that run chats or one
 * shared with the workspace, a repository that machine has for the workspace, and an agent signed
 * in there for it. `choice` is
 * the pick once it is whole. Back from `away`, such as Settings, where an agent may have been
 * signed in, it asks the machine again.
 */
export function useMyAgent(socket: WorkspaceSocket, machines: Callable[], away: boolean) {
  const [picked, setPicked] = useState<MachineId>();
  const machine = machines.find(({ id }) => id === picked) ?? machines[0];
  const id = machine?.id;
  const key = `${id} ${socket.workspace}`;
  const settings = useLoaded(() => (id === undefined ? answered<AgentKind[] | undefined>(undefined) : agents(socket, id, later)), key);
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
  const shownRepos = listed.value;
  const chosenRepo = shownRepos?.find((one) => one === repo) ?? shownRepos?.[0];
  // Once the machine answers, only the agents signed in there for this workspace are offered.
  const offered = offeredAgents(settings.value);
  const chosenAgent = offered.find((kind) => kind === agent) ?? offered[0];
  const choice: Choice | undefined =
    machine === undefined || chosenRepo === undefined || chosenAgent === undefined ? undefined : { machine: machine.id, repo: chosenRepo, agent: chosenAgent };
  const missing = machine === undefined ? undefined : agentMissing({ repos: shownRepos, agent: chosenAgent !== undefined });
  return { machines, machine, settings, listed, shownRepos, chosenRepo, offered, chosenAgent, choice, missing, setPicked, setRepo, setAgent };
}

/** The fields of `useMyAgent`'s pick, and what it still lacks. */
export function MyAgentFields({ pick, workspace, openSettings }: { pick: ReturnType<typeof useMyAgent>; workspace: string; openSettings(machine?: MachineId): void }) {
  const { machines, machine, settings, listed, shownRepos, chosenRepo, offered, chosenAgent, missing } = pick;
  return (
    <fieldset className="my-agent">
      <legend>Your agent in this chat</legend>
      <label>
        Machine
        <select value={machine?.id} onChange={(event) => pick.setPicked(event.target.value)}>
          {machines.map((one) => (
            <option key={one.id} value={one.id}>
              {machineLabel(one)}
            </option>
          ))}
        </select>
      </label>
      {shownRepos !== undefined && shownRepos.length > 0 && (
        <label>
          Repository
          <select value={chosenRepo} onChange={(event) => pick.setRepo(event.target.value)}>
            {shownRepos.map((one) => (
              <option key={one} value={one}>
                {one}
              </option>
            ))}
          </select>
        </label>
      )}
      {offered.length > 0 && (
        <label>
          Agent
          <select value={chosenAgent} onChange={(event) => pick.setAgent(offered.find((kind) => kind === event.target.value) ?? "claude")}>
            {offered.map((kind) => (
              <option key={kind} value={kind}>
                {vendorName(kind)}
              </option>
            ))}
          </select>
        </label>
      )}
      {shownRepos?.length === 0 && (
        <p className="muted">
          {machine?.own === false ? (
            "Its owner hasn't added a repository on this machine for this workspace."
          ) : (
            <>
              No repository is added on this machine for this workspace. Run <code>weacon repo add --workspace {workspace} &lt;name&gt; &lt;folder&gt;</code> on
              it, then try again.
            </>
          )}{" "}
          <button type="button" onClick={() => void listed.load()}>
            Try again
          </button>
        </p>
      )}
      {listed.problem !== undefined && <Problem text={listed.problem} retry={() => void listed.load()} />}
      {settings.problem !== undefined && <Problem text={settings.problem} retry={() => void settings.load()} />}
      {settings.value !== undefined && offered.length < KINDS.length && (
        <p className="muted">
          {offered.length === 0 && missing}{" "}
          {machine?.own === false ? (
            offered.length === 0 && "Its owner signs agents in."
          ) : (
            <button type="button" onClick={() => openSettings(machine?.id)}>
              Sign in from Settings
            </button>
          )}
        </p>
      )}
      {pick.choice !== undefined && <p className="muted">Type @ in your message to call it.</p>}
    </fieldset>
  );
}
