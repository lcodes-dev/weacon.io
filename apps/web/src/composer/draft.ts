/** An agent a message can mention, which calls it. */
export type MentionItem = { id: string; label: string };

/** A picked item, kept as the text `@label` at `[start, end)` of the draft. */
export type Mention = { start: number; end: number; item: MentionItem };

/** What the box holds: its text, and the mentions in it, in order. */
export type Draft = { text: string; mentions: readonly Mention[] };

/** A draft and where the caret goes in it. */
export type Placed = { draft: Draft; caret: number };

/** The `@` being typed before the caret: where it starts, and the text after it. */
export type Query = { start: number; text: string };

export const EMPTY_DRAFT: Draft = { text: "", mentions: [] };

/**
 * The draft after the box's text became `next`, with the caret at `caret`. An edit that reaches
 * into a mention removes all of it, so a mention is never left half typed. While an input method
 * composes, the text must stay as it is, so such a mention becomes plain text instead.
 */
export function editDraft(draft: Draft, next: string, caret: number, composing = false): Placed {
  const { text } = draft;
  // What was typed ends at the caret; the unchanged text around it bounds the edit.
  const back = Math.min(next.length - caret, commonSuffix(text, next));
  const front = Math.min(caret, text.length - back, commonPrefix(text, next));
  const inserted = next.slice(front, next.length - back);
  let from = front;
  let to = text.length - back;
  const touched = (mention: Mention) => mention.start < to && from < mention.end;
  for (const mention of composing ? [] : draft.mentions.filter(touched)) {
    from = Math.min(from, mention.start);
    to = Math.max(to, mention.end);
  }
  const shift = inserted.length - (to - from);
  const mentions = draft.mentions
    .filter((mention) => !touched(mention))
    .map((mention) => (mention.start >= to ? { ...mention, start: mention.start + shift, end: mention.end + shift } : mention));
  return {
    draft: { text: text.slice(0, from) + inserted + text.slice(to), mentions },
    caret: from + inserted.length,
  };
}

/** The `@` query the caret is at the end of, unless that is inside a mention. */
export function query(draft: Draft, caret: number): Query | undefined {
  const text = /(?:^|\s)[@＠]([^\s@＠]*)$/u.exec(draft.text.slice(0, caret))?.[1];
  if (text === undefined) {
    return undefined;
  }
  const start = caret - text.length - 1;
  return draft.mentions.some((mention) => mention.start <= start && start < mention.end) ? undefined : { start, text };
}

/** The draft with `item` in place of the query, and a space after it. */
export function pickMention(draft: Draft, at: Query, item: MentionItem): Placed {
  const mention = `@${item.label}`;
  const end = at.start + 1 + at.text.length;
  const placed = editDraft(draft, draft.text.slice(0, at.start) + mention + " " + draft.text.slice(end), at.start + mention.length + 1);
  const added = { start: at.start, end: at.start + mention.length, item };
  const mentions = [...placed.draft.mentions, added].sort((a, b) => a.start - b.start);
  return { draft: { ...placed.draft, mentions }, caret: placed.caret };
}

/**
 * The draft a box opens with: the mention of the writer's `agent` and a space while their
 * messages go to it (`calling`), which they delete to write to the chat's people; else nothing.
 */
export function opening(agent: MentionItem | undefined, calling: boolean): Placed {
  return agent !== undefined && calling ? pickMention(EMPTY_DRAFT, { start: 0, text: "" }, agent) : { draft: EMPTY_DRAFT, caret: 0 };
}

/** Whether `draft` says nothing yet: it is blank, or holds only mentions. */
export function unwritten(draft: Draft): boolean {
  const said = draft.mentions.reduceRight((text, { start, end }) => text.slice(0, start) + text.slice(end), draft.text);
  return said.trim() === "";
}

function commonPrefix(a: string, b: string): number {
  let at = 0;
  while (at < a.length && at < b.length && a[at] === b[at]) at += 1;
  return at;
}

function commonSuffix(a: string, b: string): number {
  let at = 0;
  while (at < a.length && at < b.length && a[a.length - 1 - at] === b[b.length - 1 - at]) at += 1;
  return at;
}
