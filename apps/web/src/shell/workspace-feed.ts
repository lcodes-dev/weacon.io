import type { Asks } from "../connection.ts";
import type { WorkspaceId } from "../protocol.ts";

/** What a workspace's feed asks of the server: its changes, chats' included. */
export const FEED: Asks = { needs: "workspace.feed", offers: ["workspace.chats"] };

/** The WebSocket of `workspace`'s changes after `since`, on this page's own server. */
export function feedUrl(location: { protocol: string; host: string }, workspace: WorkspaceId, since: number): string {
  const scheme = location.protocol === "https:" ? "wss" : "ws";
  return `${scheme}://${location.host}/api/workspaces/${workspace}/feed?since=${since}`;
}

// The longest wait before the feed is opened again.
const MOST_MS = 30_000;

/** How long to wait before opening the feed again after `failed` tries in a row. */
export function retryAfter(failed: number): number {
  return Math.min(MOST_MS, 1_000 * 2 ** Math.max(0, failed - 1));
}
