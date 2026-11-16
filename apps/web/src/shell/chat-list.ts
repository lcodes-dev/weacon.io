import type { Names } from "../members/names.ts";
import type { ChatLeg, ChatState, ChatSummary, MachineId, RunState } from "../protocol.ts";

/** `chats`, the one last written in or whose turn ended last first. */
export function newestFirst(chats: readonly ChatSummary[]): ChatSummary[] {
  return [...chats].sort((a, b) => b.updated_at - a.updated_at || a.id.localeCompare(b.id));
}

/** An agent a person may call in a chat: on a machine of their `own`, or on one `shared` with the workspace, which may be both. */
export type Reached = { leg: ChatLeg; own: boolean; shared: boolean };

/**
 * The agents a person may call in `chat`, whoever called each last: those it has on their `own`
 * machines first, then on the others `shared` with the workspace.
 */
export function reached(own: readonly MachineId[], shared: readonly MachineId[], chat: Pick<ChatSummary, "legs"> | undefined): Reached[] {
  const machines = [...own, ...shared.filter((machine) => !own.includes(machine))];
  const on = (machine: MachineId) => chat?.legs.filter((leg) => leg.machine_id === machine && !leg.detached) ?? [];
  return machines.flatMap((machine) => on(machine).map((leg) => ({ leg, own: own.includes(machine), shared: shared.includes(machine) })));
}

const NOTES: Partial<Record<ChatState, string>> = { full: "Full" };

/** What a chat's row says about why it takes no messages, if it doesn't. */
export function stateNote(state: ChatState): string | undefined {
  return NOTES[state];
}

// A run that sleeps wakes on the next prompt, so to a person it is idle.
const RUNS: Record<RunState, string> = {
  starting: "Starting",
  running: "Running",
  waiting: "Waiting for a person",
  idle: "Idle",
  sleeping: "Idle",
  ended: "Ended",
};

/**
 * What a chat's row says under its title: who started it, once their name is known, what its latest
 * run is doing, and its state's note; a run state this app doesn't know is left out.
 */
export function rowNote(chat: ChatSummary, names: Names): string {
  const run = chat.run !== undefined && Object.hasOwn(RUNS, chat.run) ? RUNS[chat.run] : undefined;
  return [names.get(chat.owner), run, stateNote(chat.state)].filter((part) => part !== undefined).join(" · ");
}
