import type { MemberId } from "../protocol.ts";
import type { Row } from "../transcript/transcript.ts";
import type { Ask, Chat } from "./chat.ts";
import { label } from "./label.ts";

/** One line of a chat's transcript, under a key that names it as long as the chat is shown. */
export type Line =
  | { key: string; kind: "prompt"; text: string }
  | { key: string; kind: "agent"; label: string }
  | { key: string; kind: "row"; row: Row; writing: boolean }
  | { key: string; kind: "ask"; run: string; ask: Ask };

/** The lines `chat` shows: each prompt, and each turn under its agent's label with its prompts in place. */
export function lines(chat: Chat, members: ReadonlyMap<MemberId, string>): Line[] {
  return chat.items.flatMap((item): Line[] => {
    if (item.kind === "prompt") {
      return [{ key: `p${item.seq}`, kind: "prompt", text: item.text }];
    }
    const { key, run, transcript, asks } = item.turn;
    const shown: Line[] = [{ key, kind: "agent", label: label(chat.runs.get(run)?.agent, members) }];
    const asksAt = (at: number) => asks.filter((ask) => ask.at === at).map((ask): Line => ({ key: `${key}.a${ask.prompt}`, kind: "ask", run, ask }));
    transcript.rows.forEach((row, at) => {
      shown.push(...asksAt(at), { key: `${key}.${at}`, kind: "row", row, writing: transcript.writing && at === transcript.rows.length - 1 });
    });
    return [...shown, ...asksAt(transcript.rows.length)];
  });
}
