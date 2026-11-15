import { useEffect, useState } from "react";

import type { Later } from "../connection.ts";
import type { AccessLevel, AgentKind, MachineOs, MachineSettings, MachineSettingsAction, MachineSummary, SignInState } from "../protocol.ts";
import { useAsk } from "../api/ask.ts";
import { useLoaded } from "../api/loaded.ts";
import { Problem } from "../api/problem.tsx";
import { type LinkSocket, terminalLink } from "../terminal/link.ts";
import { TerminalView } from "../terminal/terminal-view.tsx";
import type { MachineSocket } from "./ask-machine.ts";
import { isOutdated } from "./core.ts";
import type { Pins } from "./pins.ts";
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
  pins: Pins;
  /** Whether the server relays sign-in terminals. */
  relays: boolean;
  /** Whether other people are in the workspace, once its members are known. */
  team: boolean | undefined;
};

/**
 * What one of the person's machines keeps for the open workspace, asked of its core: error
 * reports, what the workspace's agents may do there, and whether they are signed in.
 */
export function MachineSettingsView({ machine, socket, later, pins, relays, team }: Props) {
  const settings = useLoaded(() => current(socket, machine.id, later), `${machine.id} ${socket.workspace}`);
  const [signingIn, setSigningIn] = useState<AgentKind>();
  const changing = useAsk();
  const set = (action: MachineSettingsAction) =>
    void changing.ask(
      () => change(socket, machine.id, action, later),
      (after) => settings.change(() => after),
    );
  const shown = settings.value;
  // The machine keeps whether others are in the workspace; it is told when its mark is out of date.
  const stale = shown !== undefined && team !== undefined && shown.team !== team;
  useEffect(() => {
    if (stale && !changing.busy) {
      set({ kind: "set_access", level: shown.access, team });
    }
  }, [stale, changing.busy]);
  return (
    <section aria-label={`${machine.name} settings`}>
      <h3>{machine.name}</h3>
      {isOutdated(machine) && <p role="status">Its weacon is older than this server supports, so some settings may not reach it.</p>}
      {settings.loading && <p className="muted">Loading</p>}
      {settings.problem !== undefined && <Problem text={settings.problem} retry={() => void settings.load()} />}
      {shown !== undefined && (
        <Shown
          settings={shown}
          os={machine.os}
          busy={changing.busy}
          set={set}
          team={team ?? shown.team}
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
            pins={pins}
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
  os: MachineOs;
  busy: boolean;
  set(action: MachineSettingsAction): void;
  /** Whether other people are in the workspace. */
  team: boolean;
  /** Opens a sign-in terminal for an agent; none while one is open or none can be. */
  signIn: ((agent: AgentKind) => void) | undefined;
};

function Shown({ settings, os, busy, set, team, signIn }: ShownProps) {
  const { reports, access } = settings;
  return (
    <>
      <h4>Error reports</h4>
      <p className="muted">Reports say what went wrong in this machine's core. They hold no messages, prompts, code or file contents.</p>
      <p>
        <span>{reports === undefined ? "Not chosen yet, so none are sent." : reports ? "On." : "Off."}</span>{" "}
        {reports !== true && (
          <button type="button" disabled={busy} onClick={() => set({ kind: "set_reports", send: true })}>
            Turn on
          </button>
        )}
        {reports !== false && (
          <button type="button" disabled={busy} onClick={() => set({ kind: "set_reports", send: false })}>
            {reports === undefined ? "Keep off" : "Turn off"}
          </button>
        )}
      </p>
      <h4>Agent access</h4>
      <label>
        What this workspace's agents may do on this machine
        <select
          value={access}
          disabled={busy}
          onChange={(event) => {
            const level = LEVELS.find(([value]) => value === event.target.value)?.[0];
            if (level !== undefined) {
              set({ kind: "set_access", level, team });
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
      {access === "full" && os === "windows" && (
        <p>On Windows the command guard needs a POSIX shell, such as Git Bash, which can't be assured here, so at full access Claude Code still asks before every command — the same as ask first. Codex has no such guard.</p>
      )}
      {access === "full" && os !== "windows" && (
        <p>Even at full access, Claude Code still asks before it reaches outside the workspace or an agent's sign-ins. It catches mistakes, not an agent that is trying, and is not a sandbox. Codex has no such guard.</p>
      )}
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
