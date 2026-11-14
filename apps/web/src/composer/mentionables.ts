import type { MentionItem, Query } from "./draft.ts";

// How many matches the picker offers at once.
const SHOWN = 5;

/** Who a message can mention: made up until workspaces list their own people and agents. */
export function mentionables(): readonly MentionItem[] {
  return [
    { kind: "person", id: "m-mira", label: "Mira Okafor" },
    { kind: "person", id: "m-jun", label: "Jun Sato" },
    { kind: "person", id: "m-lena", label: "Lena Brandt" },
    { kind: "person", id: "m-wei", label: "王伟" },
    { kind: "agent", id: "a-claude", label: "Claude Code" },
    { kind: "agent", id: "a-codex", label: "Codex" },
  ];
}

/**
 * What the picker offers for the query `at`: nothing once it was dismissed, or while a message
 * is being sent, as a pick then would be lost when the box clears.
 */
export function offered(at: Query | undefined, dismissed: number | undefined, sending: boolean): MentionItem[] {
  return at === undefined || at.start === dismissed || sending ? [] : matching(mentionables(), at.text);
}

/** The items with a word in their label that starts with `text`, ignoring case. */
export function matching(items: readonly MentionItem[], text: string): MentionItem[] {
  const wanted = text.toLocaleLowerCase();
  return items
    .filter(({ label }) => label.toLocaleLowerCase().split(/\s+/u).some((word) => word.startsWith(wanted)))
    .slice(0, SHOWN);
}
