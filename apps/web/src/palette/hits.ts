import type { SearchHit, Snippet } from "../protocol.ts";
import type { Place } from "../shell/place.ts";

/** A hit as the palette lists it: what it is, its title and snippet, and the place it opens. A message opens its chat at it. */
export type Listed = { key: string; kind: "Task" | "Doc" | "Chat"; title: string; snippet: Snippet; place: Place };

/** The hits the palette knows how to list, in the server's order; a newer kind is left out. */
export function listed(hits: SearchHit[]): Listed[] {
  return hits.flatMap((hit): Listed[] => {
    switch (hit.kind) {
      case "task":
        return [{ key: `task:${hit.task}`, kind: "Task", title: hit.title, snippet: hit.snippet, place: { view: "tasks", task: hit.task } }];
      case "doc":
        return [{ key: `doc:${hit.doc}`, kind: "Doc", title: hit.title, snippet: hit.snippet, place: { view: "docs", doc: hit.doc } }];
      case "chat":
        return [
          {
            key: `chat:${hit.chat}:${hit.seq ?? ""}`,
            kind: "Chat",
            title: hit.title,
            snippet: hit.snippet,
            place: hit.seq === undefined ? { view: "chats", chat: hit.chat } : { view: "chats", chat: hit.chat, at: hit.seq },
          },
        ];
      default:
        return [];
    }
  });
}

/** `snippet`'s text cut at its marks, each part saying whether it matched. */
export function parts(snippet: Snippet): { text: string; marked: boolean }[] {
  const cut: { text: string; marked: boolean }[] = [];
  let at = 0;
  for (const { start, end } of snippet.marks) {
    if (start < at || end < start) {
      continue;
    }
    cut.push({ text: snippet.text.slice(at, start), marked: false }, { text: snippet.text.slice(start, end), marked: true });
    at = end;
  }
  cut.push({ text: snippet.text.slice(at), marked: false });
  return cut.filter((part) => part.text !== "");
}

/** Whether `event` is the key that opens the palette: ⌘K, or Ctrl+K off a Mac. */
export function opensPalette(event: Pick<KeyboardEvent, "key" | "metaKey" | "ctrlKey">): boolean {
  return event.key.toLowerCase() === "k" && (event.metaKey || event.ctrlKey);
}
