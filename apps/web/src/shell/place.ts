import { isId } from "../ids.ts";
import type { ChatId, DocId, WorkspaceId } from "../protocol.ts";

/** Where in a workspace the page is: a chat, the tasks, a doc or the docs, or the proposals. */
export type Place = { view: "chats"; chat?: ChatId } | { view: "tasks" } | { view: "docs"; doc?: DocId } | { view: "proposals" };

const VIEWS = new Set(["tasks", "docs", "proposals"]);

/** The place `?view=`, `?chat=` and `?doc=` name; anything else is the chats. */
export function placeOf(search: string): Place {
  const params = new URLSearchParams(search);
  const view = params.get("view") ?? "chats";
  const id = (key: string) => {
    const value = params.get(key);
    return value !== null && isId(value) ? value : undefined;
  };
  if (!VIEWS.has(view)) {
    const chat = id("chat");
    return chat === undefined ? { view: "chats" } : { view: "chats", chat };
  }
  const doc = id("doc");
  return view === "docs" && doc !== undefined ? { view, doc } : ({ view } as Place);
}

/** The address query of `place` in `workspace`. */
export function searchOf(workspace: WorkspaceId | undefined, place: Place): string {
  if (workspace === undefined) {
    return "";
  }
  const params = new URLSearchParams({ workspace });
  if (place.view === "chats") {
    if (place.chat !== undefined) {
      params.set("chat", place.chat);
    }
  } else {
    params.set("view", place.view);
    if (place.view === "docs" && place.doc !== undefined) {
      params.set("doc", place.doc);
    }
  }
  return `?${params.toString()}`;
}
