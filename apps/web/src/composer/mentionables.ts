import { vendorName } from "../chat/label.ts";
import type { AgentKind } from "../protocol.ts";
import type { MentionItem, Query } from "./draft.ts";

/** The writer's own agent of `kind`, as a message mentions it to call it. */
export function agentItem(kind: AgentKind): MentionItem {
  return { id: kind, label: vendorName(kind) };
}

/**
 * What the picker offers for the query `at`: the writer's own `agent` in the chat, when a word of
 * its name starts with the query, ignoring case. Nothing once it was dismissed, or while a message
 * is being sent, as a pick then would be lost when the box clears.
 */
export function offered(at: Query | undefined, dismissed: number | undefined, sending: boolean, agent: MentionItem | undefined): MentionItem[] {
  if (at === undefined || at.start === dismissed || sending || agent === undefined) {
    return [];
  }
  const wanted = at.text.toLocaleLowerCase();
  const named = agent.label.toLocaleLowerCase().split(/\s+/u);
  return named.some((word) => word.startsWith(wanted)) ? [agent] : [];
}
