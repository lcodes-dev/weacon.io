import type { ChatRef } from "./sync.ts";

/** The part of `localStorage` this file uses. */
export type Store = Pick<Storage, "getItem" | "setItem">;

const KEY = "weacon.tabs";

/** The chat chosen for the tab `tab`, if one was. */
export function chosen(store: Store, tab: number): ChatRef | undefined {
  return read(store)[String(tab)];
}

/** Remembers `ref` as the chat for `tab`. */
export function choose(store: Store, tab: number, ref: ChatRef): void {
  store.setItem(KEY, JSON.stringify({ ...read(store), [String(tab)]: ref }));
}

/** Forgets the chats of the tabs not in `open`; whenever it runs, no open tab's choice is lost. */
export function forget(store: Store, open: readonly number[]): void {
  const kept = Object.entries(read(store)).filter(([at]) => open.includes(Number(at)));
  store.setItem(KEY, JSON.stringify(Object.fromEntries(kept)));
}

// What another build or a damaged store left is dropped, entry by entry.
function read(store: Store): Record<string, ChatRef> {
  let value: unknown;
  try {
    value = JSON.parse(store.getItem(KEY) ?? "{}");
  } catch {
    return {};
  }
  if (typeof value !== "object" || value === null) {
    return {};
  }
  const refs = Object.entries(value).filter(
    (entry): entry is [string, ChatRef] => typeof entry[1]?.workspace === "string" && typeof entry[1]?.chat === "string",
  );
  return Object.fromEntries(refs.map(([tab, { workspace, chat }]) => [tab, { workspace, chat }]));
}
