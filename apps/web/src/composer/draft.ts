/** Someone or something a message can mention. The kinds aren't settled, so `kind` is open. */
export type MentionItem = { kind: string; id: string; label: string };

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
  const back = Math.min(next.length - caret, common(text, next, (at) => text.length - 1 - at, (at) => next.length - 1 - at));
  const front = Math.min(caret, text.length - back, common(text, next, (at) => at, (at) => at));
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

// How many characters match, walking `text` by `a` and `next` by `b`.
function common(text: string, next: string, a: (at: number) => number, b: (at: number) => number): number {
  let at = 0;
  while (at < text.length && at < next.length && text[a(at)] === next[b(at)]) {
    at += 1;
  }
  return at;
}
