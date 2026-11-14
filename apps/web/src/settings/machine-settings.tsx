import { useEffect, useState } from "react";

import { type Link, type Place, type Wait, change, current } from "../machines/link.ts";
import type { Pins } from "../machines/pins.ts";
import type { AccessLevel, AgentKind, MachineSettings, MachineSettingsAction, MachineSummary, SignInState } from "../protocol.ts";
import { useAsk } from "../shell/ask.ts";
import { useLoaded } from "../shell/loaded.ts";

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

const timer: Wait = (run, ms) => {
  const started = setTimeout(run, ms);
  return () => clearTimeout(started);
};

/**
 * What one of the person's machines keeps for the open workspace, asked of its core: error
 * reports, what the workspace's agents may do there, and whether they are signed in.
 */
export function MachineSettingsView({
  machine,
  workspace,
  link,
  pins,
  signIn,
}: {
  machine: MachineSummary;
  workspace: string;
  link: Link;
  pins: Pins;
  signIn?: (agent: AgentKind) => void;
}) {
  const place: Place = { machine: machine.id, workspace };
  const settings = useLoaded(() => current(link, place, timer), `${machine.id} ${workspace}`);
  const changing = useAsk();
  const set = (action: MachineSettingsAction) =>
    void changing.ask(
      () => change(link, place, action, timer),
      (after) => settings.change(() => after),
    );
  const shown = settings.value;
  return (
    <section aria-label={`${machine.name} settings`}>
      <h3>{machine.name}</h3>
      {shown === undefined && settings.problem === undefined && <p className="muted">Loading</p>}
      {settings.problem !== undefined && (
        <p role="alert">
          {settings.problem}{" "}
          <button type="button" onClick={() => void settings.load()}>
            Try again
          </button>
        </p>
      )}
      {shown !== undefined && <Shown settings={shown} busy={changing.busy} set={set} machine={machine} pins={pins} signIn={signIn} />}
      {changing.problem !== undefined && <p role="alert">{changing.problem}</p>}
    </section>
  );
}

function Shown({
  settings,
  busy,
  set,
  machine,
  pins,
  signIn,
}: {
  settings: MachineSettings;
  busy: boolean;
  set(action: MachineSettingsAction): void;
  machine: MachineSummary;
  pins: Pins;
  signIn: ((agent: AgentKind) => void) | undefined;
}) {
  const { reports, access, team } = settings;
  return (
    <>
      <h4>Error reports</h4>
      <p>
        {reports === undefined
          ? "Not chosen yet, so none are sent."
          : reports
            ? "On. They hold no messages, prompts, code or file contents."
            : "Off."}{" "}
        <button type="button" disabled={busy} onClick={() => set({ kind: "set_reports", send: reports !== true })}>
          {reports === true ? "Turn off" : "Turn on"}
        </button>
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
      <label>
        <input
          type="checkbox"
          checked={team}
          disabled={busy}
          onChange={(event) => set({ kind: "set_access", level: access, team: event.target.checked })}
        />
        Other people are in this workspace
      </label>
      {team && access === "full" && (
        <p role="alert">Full access is on in a team workspace. Its agents change files on this machine without asking.</p>
      )}
      <h4>Sign-ins</h4>
      <SignIns settings={settings} machine={machine} pins={pins} signIn={signIn} />
    </>
  );
}

function SignIns({
  settings,
  machine,
  pins,
  signIn,
}: {
  settings: MachineSettings;
  machine: MachineSummary;
  pins: Pins;
  signIn: ((agent: AgentKind) => void) | undefined;
}) {
  // Only a key this browser pinned when the person confirmed the pairing opens a sign-in.
  const [trusted, setTrusted] = useState<boolean>();
  useEffect(() => {
    let current = true;
    void pins.matches(machine.id, machine.key).then(
      (check) => current && setTrusted(check === "match"),
      () => current && setTrusted(false),
    );
    return () => {
      current = false;
    };
  }, [pins, machine.id, machine.key]);
  return (
    <>
      <ul aria-label="Sign-ins">
        {settings.sign_ins.map(({ agent, state }) => (
          <li key={agent}>
            {AGENTS.get(agent) ?? "Another agent"}: {STATES.get(state) ?? "Unknown"}{" "}
            {trusted === true && signIn !== undefined && state !== "not_installed" && (
              <button type="button" onClick={() => signIn(agent)}>
                Sign in
              </button>
            )}
          </li>
        ))}
      </ul>
      {trusted === false && <p className="muted">This browser hasn't confirmed this machine's key.</p>}
    </>
  );
}
