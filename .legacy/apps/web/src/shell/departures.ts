import type { WorkspaceId } from "../protocol.ts";

/**
 * The workspaces the person left or was told they are out of. A refusal of one is no news, since
 * the list, reloaded since, no longer names it, until the list names it again.
 */
export class Departures {
  #gone = new Set<WorkspaceId>();

  /** Notes that the person is out of `workspace`; whether that is news. */
  leave(workspace: WorkspaceId): boolean {
    const news = !this.#gone.has(workspace);
    this.#gone.add(workspace);
    return news;
  }

  /** Takes the workspaces the list names now: the person is back in those. */
  listed(workspaces: readonly WorkspaceId[]): void {
    for (const workspace of workspaces) {
      this.#gone.delete(workspace);
    }
  }
}
