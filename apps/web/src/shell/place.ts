import { isId } from "../ids.ts";
import type { ChatId, DocId, TaskId, WorkspaceId } from "../protocol.ts";

/**
 * Where in a workspace the page is: a chat, at the message `at` in its log if one is named, or New
 * chat for a `task`; the tasks, with one shown; a doc or the docs; or the proposals.
 */
export type Place = { view: "chats"; chat?: ChatId; at?: number; task?: TaskId } | { view: "tasks"; task?: TaskId } | { view: "docs"; doc?: DocId } | { view: "proposals" };

const VIEWS = new Set(["tasks", "docs", "proposals"]);

/** The place `?view=`, `?chat=`, `?at=`, `?task=` and `?doc=` name; anything else is the chats. */
export function placeOf(search: string): Place {
  const params = new URLSearchParams(search);
  const view = params.get("view") ?? "chats";
  const id = (key: string) => {
    const value = params.get(key);
    return value !== null && isId(value) ? value : undefined;
  };
  const task = id("task");
  if (!VIEWS.has(view)) {
    const chat = id("chat");
    if (chat === undefined) {
      return task === undefined ? { view: "chats" } : { view: "chats", task };
    }
    const at = Number(params.get("at") ?? "");
    return Number.isSafeInteger(at) && at > 0 ? { view: "chats", chat, at } : { view: "chats", chat };
  }
  if (view === "tasks") {
    return task === undefined ? { view } : { view, task };
  }
  const doc = id("doc");
  return view === "docs" && doc !== undefined ? { view, doc } : ({ view } as Place);
}

/** Whether a click on a link is one the page follows itself, rather than one the browser opens elsewhere. */
export function followsHere(click: { button: number; metaKey: boolean; ctrlKey: boolean; shiftKey: boolean; altKey: boolean }): boolean {
  return click.button === 0 && !click.metaKey && !click.ctrlKey && !click.shiftKey && !click.altKey;
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
      if (place.at !== undefined) {
        params.set("at", String(place.at));
      }
    } else if (place.task !== undefined) {
      params.set("task", place.task);
    }
  } else {
    params.set("view", place.view);
    if (place.view === "docs" && place.doc !== undefined) {
      params.set("doc", place.doc);
    }
    if (place.view === "tasks" && place.task !== undefined) {
      params.set("task", place.task);
    }
  }
  return `?${params.toString()}`;
}
