import type { Names } from "../members/names.ts";
import type { ChatState, ChatSummary, MemberId, RunState } from "../protocol.ts";

/** `chats`, the one whose latest turn began or ended last first. */
export function newestFirst(chats: readonly ChatSummary[]): ChatSummary[] {
  return [...chats].sort((a, b) => b.updated_at - a.updated_at || a.id.localeCompare(b.id));
}

/** Whether `member`, the person's in the chat's workspace, owns `chat`; an unknown member owns none. */
export function owns(member: MemberId | undefined, chat: ChatSummary | undefined): boolean {
  return member !== undefined && chat?.owner === member;
}

const NOTES: Partial<Record<ChatState, string>> = { full: "Full", detached: "Read only" };

/** What a chat's row says about why it takes no prompts, if it doesn't. */
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
 * What a chat's row says under its title: its owner's name once known, its machine, what its latest
 * run is doing, and its state's note; a run state this app doesn't know is left out.
 */
export function rowNote(chat: ChatSummary, names: Names): string {
  const run = chat.run !== undefined && Object.hasOwn(RUNS, chat.run) ? RUNS[chat.run] : undefined;
  return [names.get(chat.owner), chat.machine, run, stateNote(chat.state)].filter((part) => part !== undefined).join(" · ");
}
