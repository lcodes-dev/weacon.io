import { useEffect, useState } from "react";

import type { ChatRef } from "../chat-ref.ts";
import { ChatScreen } from "../chat/chat-screen.tsx";
import { type Asks, Connection, type Status, later, statusText } from "../connection.ts";
import type { ChatLeg } from "../protocol.ts";
import { ME } from "../me.ts";
import { Settings } from "../settings/settings.tsx";
import { useSocketFor } from "../shell/workspace-socket.ts";

const TRANSCRIPT: Asks = { needs: "run.stream", offers: ["diff.view", "workspace.chats", "workspace.chat_commands", "worktree.manage", "worktree.open_terminal"] };
// The fake server keeps one workspace; a page without a chat in its address still needs a name for it.
const FAKE_WORKSPACE = "w-1";
// The fake server's one person, with their agent in its one chat.
const FAKE_LEG: ChatLeg = { member: "m-1", machine_id: "mc-1", machine: "laptop", agent: "claude", detached: false };

/**
 * The chat at `chatRef` on the fake server, over a connection of its own behind a workspace
 * socket, for development and the end-to-end tests. Its one person owns its one chat.
 */
export function ChatPage({ url, chatRef }: { url: string; chatRef: ChatRef | undefined }) {
  const [status, setStatus] = useState<Status>({ state: "connecting" });
  const [settings, setSettings] = useState(false);
  const socket = useSocketFor(chatRef?.workspace ?? FAKE_WORKSPACE);
  useEffect(() => {
    let opened: WebSocket;
    try {
      opened = new WebSocket(url);
    } catch {
      // Only a malformed `?server=` gets here.
      setStatus({ state: "ended", code: "connection.refused" });
      return;
    }
    const connection = new Connection(
      opened,
      ME,
      {
        status(next) {
          setStatus(next);
          if (next.state === "live") {
            socket.attach((envelope) => connection.send(envelope), next.capabilities);
          } else if (next.state === "ended") {
            socket.attach(undefined);
          }
        },
        message: (envelope) => socket.heard(envelope),
      },
      TRANSCRIPT,
      later,
    );
    return () => {
      socket.attach(undefined);
      connection.close();
    };
  }, [url, socket]);
  return (
    <div className="app">
      <header>
        <span className="status">{statusText(status)}</span>
        <button type="button" onClick={() => setSettings(!settings)}>
          {settings ? "Back" : "Settings"}
        </button>
      </header>
      <main>
        <div className={settings ? "pane away" : "pane"}>
          <ChatScreen link={socket} live={status.state === "live"} chatRef={chatRef} me={FAKE_LEG.member} agents={[{ leg: FAKE_LEG, own: true, shared: false }]} machines={[]} missing={undefined} openSettings={() => {}} away={settings} />
        </div>
        {settings && (
          <div className="pane">
            <Settings me={ME} server={status.state === "live" ? status.server : undefined} />
          </div>
        )}
      </main>
    </div>
  );
}
