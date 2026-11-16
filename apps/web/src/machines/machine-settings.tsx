import { useState } from "react";

import type { Later } from "../connection.ts";
import type { AccessLevel, AgentKind, MachineSettings, MachineSettingsAction, MachineSummary, SignInState } from "../protocol.ts";
import { useAsk } from "../api/ask.ts";
import { useLoaded } from "../api/loaded.ts";
import { Problem } from "../api/problem.tsx";
import { type LinkSocket, terminalLink } from "../terminal/link.ts";
import { TerminalView } from "../terminal/terminal-view.tsx";
import type { MachineSocket } from "./ask-machine.ts";
import { isOutdated } from "./core.ts";
import { change, current } from "./remote-settings.ts";

const LEVELS: Array<[AccessLevel, string]> = [
  ["off", "Off"],
  ["read_only", "Read only"],
  ["ask_first", "Ask first"],
  ["full", "Full"],
];

const AGENTS = new Map<AgentKind | string, string>([
  ["claude", "Claude Code"],
  ["codex", "Codex"],
]);

const STATES = new Map<SignInState | string, string>([
  ["signed_in", "Signed in"],
  ["signed_out", "Signed out"],
  ["not_installed", "Not installed"],
]);

type Props = {
  machine: MachineSummary;
  socket: MachineSocket & LinkSocket;
  later: Later;
  /** Whether the server relays sign-in terminals. */
  relays: boolean;
  /** Whether other people are in the workspace, once its members are known. */
  team: boolean | undefined;
};

/**
 * What one of the person's machines keeps for the open workspace, asked of its core: what the
 * workspace's agents may do there, and whether they are signed in.
 */
export function MachineSettingsView({ machine, socket, later, relays, team }: Props) {
  const settings = useLoaded(() => current(socket, machine.id, later), `${machine.id} ${socket.workspace}`);
  const [signingIn, setSigningIn] = useState<AgentKind>();
  const changing = useAsk();
  const set = (action: MachineSettingsAction) =>
    void changing.ask(
      () => change(socket, machine.id, action, later),
      (after) => settings.change(() => after),
    );
  const shown = settings.value;
  return (
    <section aria-label={`${machine.name} settings`}>
      <h3>{machine.name}</h3>
      {isOutdated(machine) && <p role="status">Its weacon is older than this server supports, so some settings may not reach it.</p>}
      {settings.loading && <p className="muted">Loading</p>}
      {settings.problem !== undefined && <Problem text={settings.problem} retry={() => void settings.load()} />}
      {shown !== undefined && (
        <Shown
          settings={shown}
          busy={changing.busy}
          set={set}
          team={team === true}
          signIn={relays && signingIn === undefined ? setSigningIn : undefined}
        />
      )}
      {changing.problem !== undefined && <p role="alert">{changing.problem}</p>}
      {signingIn !== undefined && (
        <>
          <TerminalView
            machine={machine}
            workspace={socket.workspace}
            agent={signingIn}
            {...terminalLink(socket)}
            onClosed={() => void settings.load()}
          />
          <button
            type="button"
            onClick={() => {
              setSigningIn(undefined);
              void settings.load();
            }}
          >
            Close the sign-in
          </button>
        </>
      )}
    </section>
  );
}

type ShownProps = {
  settings: MachineSettings;
  busy: boolean;
  set(action: MachineSettingsAction): void;
  /** Whether other people are in the workspace. */
  team: boolean;
  /** Opens a sign-in terminal for an agent; none while one is open or none can be. */
  signIn: ((agent: AgentKind) => void) | undefined;
};

function Shown({ settings, busy, set, team, signIn }: ShownProps) {
  const { access } = settings;
  return (
    <>
      <h4>Agent access</h4>
      <label>
        What this workspace's agents may do on this machine
        <select
          value={access}
          disabled={busy}
          onChange={(event) => {
            const level = LEVELS.find(([value]) => value === event.target.value)?.[0];
            if (level !== undefined) {
              set({ kind: "set_access", level });
            }
          }}
        >
          {LEVELS.map(([level, name]) => (
            <option key={level} value={level}>
              {name}
            </option>
          ))}
        </select>
      </label>
      {team && access === "full" && (
        <p role="alert">Full access is on in a team workspace. Its agents change files on this machine without asking.</p>
      )}
      <h4>Sharing</h4>
      <label>
        <input type="checkbox" checked={settings.shared === true} disabled={busy} onChange={(event) => set({ kind: "set_shared", shared: event.target.checked })} />{" "}
        Share this machine with this workspace
      </label>
      <p className="muted">Every member can then call agents on it. Only you answer their permission prompts, and only you change these settings.</p>
      <h4>Sign-ins</h4>
      <ul aria-label="Sign-ins">
        {settings.sign_ins.map(({ agent, state }) => (
          <li key={agent}>
            {AGENTS.get(agent) ?? "Another agent"}: {STATES.get(state) ?? "Unknown"}{" "}
            {signIn !== undefined && AGENTS.has(agent) && state !== "not_installed" && (
              <button type="button" onClick={() => signIn(agent)}>
                {state === "signed_in" ? "Sign in again" : "Sign in"}
              </button>
            )}
          </li>
        ))}
      </ul>
    </>
  );
}
