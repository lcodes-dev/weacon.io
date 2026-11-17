import { workspaceOf } from "../chat-ref.ts";
import { isId } from "../ids.ts";
import type { WorkspaceId } from "../protocol.ts";

export type Store = Pick<Storage, "getItem" | "setItem" | "removeItem">;

/** The browser's storage, which reading can itself fail when site storage is blocked. */
export type Storing = () => Store;

export const STORE: Storing = () => window.localStorage;

const KEY = "weacon.workspace";

/** The workspace to open: the one `?workspace=` names, else the last one opened in this browser. */
export function opening(search: string, store: Storing): WorkspaceId | undefined {
  return workspaceOf(search) ?? last(store);
}

/** Keeps `workspace` as the last one opened, or forgets it. */
export function remember(store: Storing, workspace: WorkspaceId | undefined): void {
  try {
    if (workspace === undefined) {
      store().removeItem(KEY);
    } else {
      store().setItem(KEY, workspace);
    }
  } catch {
    // Storage that is off or full only means the next visit starts at the list.
  }
}

function last(store: Storing): WorkspaceId | undefined {
  try {
    const id = store().getItem(KEY);
    return id !== null && isId(id) ? id : undefined;
  } catch {
    return undefined;
  }
}
