// Stands in for the server's workspace feeds, for the shell's end-to-end tests.

import type { Page } from "playwright-core";

import type { Envelope } from "../src/protocol.ts";

const CHANGE_AFTER_MS = 500;

/** What the feed stand-in does beyond welcoming each socket. */
type Feed = {
  /** What to send the nth socket for its workspace, a moment after its welcome. */
  changes?: (opened: number, workspace: string) => Envelope[];
  /** Whether to close the nth socket after its changes. */
  close?: (opened: number) => boolean;
  /** Capabilities to offer besides the feed and chats, or those the nth socket offers. */
  also?: string[] | ((opened: number) => string[]);
  /** What the nth socket answers each envelope the page sends after its hello; `reply` answers later. */
  answer?: (sent: Envelope, opened: number, reply: (envelope: Envelope) => void) => Envelope[];
  /** Whether the nth socket closes once it has answered `sent`. */
  drop?: (sent: Envelope, opened: number) => boolean;
};

/**
 * Stands in for every workspace's feed: welcomes it offering chats and `also`, then does what
 * `feed` says. Gives the addresses opened, what the page sent after its hellos, and a way to
 * close the latest socket. Only pages loaded after this have their sockets routed, so it reloads the page.
 */
export async function serveFeed(
  page: Page,
  { changes = () => [], close = () => false, also = [], answer = () => [], drop = () => false }: Feed = {},
): Promise<{ opened: string[]; sent: Envelope[]; closeLatest(): Promise<void> }> {
  const opened: string[] = [];
  const sent: Envelope[] = [];
  let latest: { close(): Promise<void> } | undefined;
  await page.routeWebSocket(/\/api\/workspaces\/[^/]+\/feed/, (socket) => {
    opened.push(new URL(socket.url()).search);
    latest = socket;
    const nth = opened.length;
    socket.onMessage((text) => {
      const hello = JSON.parse(String(text)) as Envelope;
      if (hello.type !== "hello") {
        sent.push(hello);
        const reply = (envelope: Envelope) => socket.send(JSON.stringify(envelope));
        answer(hello, nth, reply).forEach(reply);
        if (drop(hello, nth)) {
          void socket.close();
        }
        return;
      }
      const app = { name: "weacon-server", version: "0.0.0", commit: "0000000" };
      const capabilities = ["workspace.feed", "workspace.chats", ...(typeof also === "function" ? also(nth) : also)];
      const welcome: Envelope = { id: hello.id, type: "welcome", app, capabilities };
      socket.send(JSON.stringify(welcome));
      const workspace = /workspaces\/([^/]+)\/feed/.exec(socket.url())?.[1] ?? "";
      // Changes come a moment after the welcome, as a server's would once something changes.
      setTimeout(() => {
        for (const change of changes(nth, workspace)) {
          socket.send(JSON.stringify(change));
        }
        if (close(nth)) {
          void socket.close();
        }
      }, CHANGE_AFTER_MS);
    });
  });
  await page.reload();
  return { opened, sent, closeLatest: async () => latest?.close() };
}
