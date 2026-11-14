import type { Later } from "../connection.ts";
import type { AccessLevel, AgentKind, MachineSettings, MachineSettingsAction, MachineSummary, SignInState } from "../protocol.ts";
import { useAsk } from "../api/ask.ts";
import { useLoaded } from "../api/loaded.ts";
import { type Link, change, current } from "./link.ts";

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

/**
 * What one of the person's machines keeps for the open workspace, asked of its core: error
 * reports, what the workspace's agents may do there, and whether they are signed in.
 */
export function MachineSettingsView({ machine, link, later }: { machine: MachineSummary; link: Link; later: Later }) {
  const settings = useLoaded(() => current(link, machine.id, later), `${machine.id} ${link.workspace}`);
  const changing = useAsk();
  const set = (action: MachineSettingsAction) =>
    void changing.ask(
      () => change(link, machine.id, action, later),
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
      {shown !== undefined && <Shown settings={shown} busy={changing.busy} set={set} />}
      {changing.problem !== undefined && <p role="alert">{changing.problem}</p>}
    </section>
  );
}

function Shown({ settings, busy, set }: { settings: MachineSettings; busy: boolean; set(action: MachineSettingsAction): void }) {
  const { reports, access, team } = settings;
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
      <ul aria-label="Sign-ins">
        {settings.sign_ins.map(({ agent, state }) => (
          <li key={agent}>
            {AGENTS.get(agent) ?? "Another agent"}: {STATES.get(state) ?? "Unknown"}
          </li>
        ))}
      </ul>
    </>
  );
}
