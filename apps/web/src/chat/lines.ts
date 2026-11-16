import type { RunId } from "../protocol.ts";
import type { TranscriptRow } from "../transcript/transcript.ts";
import type { Ask, Chat } from "./chat.ts";
import type { Names } from "../members/names.ts";
import { label } from "./label.ts";

/** One line of a chat's transcript, under a key that names it as long as the chat is shown. */
export type Line =
  | { key: string; kind: "prompt"; text: string; author: string | undefined }
  | { key: string; kind: "agent"; label: string }
  | { key: string; kind: "row"; row: TranscriptRow; writing: boolean }
  | { key: string; kind: "ask"; run: RunId; ask: Ask };

/**
 * The lines `chat` shows: what each person wrote under their name, and each turn under its agent's
 * label, which names its owner, both by `names`, with its prompts in place.
 */
export function lines(chat: Chat, names?: Names): Line[] {
  return chat.items.flatMap((item): Line[] => {
    if (item.kind === "prompt") {
      return [{ key: `p${item.seq}`, kind: "prompt", text: item.text, author: names?.get(item.author ?? "") }];
    }
    const { key, run, transcript, asks } = item.turn;
    const shown: Line[] = [{ key, kind: "agent", label: label(chat.runs.get(run)?.agent, names) }];
    const asksAt = (at: number) => asks.filter((ask) => ask.at === at).map((ask): Line => ({ key: `${key}.a${ask.prompt}`, kind: "ask", run, ask }));
    transcript.rows.forEach((row, at) => {
      shown.push(...asksAt(at), { key: `${key}.${at}`, kind: "row", row, writing: transcript.writing && at === transcript.rows.length - 1 });
    });
    return [...shown, ...asksAt(transcript.rows.length)];
  });
}

/** The key of the line that shows the message at `seq` in `chat`'s log, once the chat holds it. */
export function lineAt(chat: Chat, seq: number): string | undefined {
  for (const item of chat.items) {
    if (item.kind === "prompt") {
      if (item.seq === seq) {
        return `p${seq}`;
      }
      continue;
    }
    const row = item.turn.texts.get(seq);
    if (row !== undefined) {
      return `${item.turn.key}.${row}`;
    }
  }
  return undefined;
}
