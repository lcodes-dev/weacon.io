import type { Names } from "../members/names.ts";
import type { Callable, Choice } from "../new-chat/start.ts";
import type { ChatLeg, ChatState, ChatSummary, MachineId, MemberId, RunState } from "../protocol.ts";

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

/** The agents of a chat `member` just started, until the list has it: the one its first message `called`, on one of `machines`. */
export function firstLeg(called: Choice | undefined, machines: readonly Callable[], member: MemberId | undefined): ChatLeg[] {
  const on = machines.find(({ id }) => id === called?.machine);
  if (called === undefined || on === undefined || member === undefined) {
    return [];
  }
  return [{ agent: called.agent, detached: false, machine: on.name, machine_id: on.id, member, repo: called.repo }];
}

/** A chat's `listed` agents with those it `started` with: the list has one only once its machine took a prompt. */
export function withFirst(listed: readonly ChatLeg[], started: readonly ChatLeg[]): ChatLeg[] {
  return [...listed, ...started.filter((leg) => !listed.some((one) => one.machine_id === leg.machine_id))];
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
 * run is doing, and its state's note.
 */
export function rowNote(chat: ChatSummary, names: Names): string {
  const run = chat.run === undefined ? undefined : RUNS[chat.run];
  return [names.get(chat.owner), run, stateNote(chat.state)].filter((part) => part !== undefined).join(" · ");
}
