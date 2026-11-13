import type { Asks } from "../connection.ts";
import type { WorkspaceId } from "../protocol.ts";

/** What a workspace's feed asks of the server. */
export const FEED: Asks = { needs: "workspace.feed", offers: [] };

/** The WebSocket of `workspace`'s changes, on this page's own server. */
export function feedUrl(location: { protocol: string; host: string }, workspace: WorkspaceId): string {
  const scheme = location.protocol === "https:" ? "wss" : "ws";
  return `${scheme}://${location.host}/api/workspaces/${workspace}/feed`;
}
