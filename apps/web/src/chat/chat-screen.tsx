import { useEffect, useState } from "react";

import type { ChatRef } from "../chat-ref.ts";
import { Composer } from "../composer/composer.tsx";
import { blocked } from "../composer/prompt.ts";
import { ChatChanges } from "../diff/chat-changes.tsx";
import { useChanges } from "../diff/use-changes.ts";
import { ReviewBar } from "../worktree/review-bar.tsx";
import { useWorktree } from "../worktree/use-worktree.ts";
import type { WorkspaceSocket } from "../shell/workspace-socket.ts";
import { useChatAccess } from "./access.ts";
import { working } from "./chat.ts";
import { useChatCommands, useLiveChat } from "./chat-hooks.ts";
import { landing } from "./landing.ts";
import type { Names } from "../members/names.ts";
import { lines } from "./lines.ts";
import { StopButton } from "./stop-button.tsx";
import { TranscriptView } from "./transcript-view.tsx";
import "./chat.css";

/**
 * The chat at `chatRef` over its workspace's `link`: its transcript, and its changes with Changes,
 * kept in place while `away` under another page such as Settings. Its owner (`mine`) also gets the message box, Stop and the permission prompts' answers, where
 * the server takes commands.
 */
export function ChatScreen({
  link,
  live,
  chatRef,
  mine,
  away,
  title,
  names,
  at,
}: {
  link: WorkspaceSocket;
  /** Whether the link's connection is up. */
  live: boolean;
  chatRef: ChatRef | undefined;
  mine: boolean;
  away: boolean;
  /** The chat's title, a pull request's to start with. */
  title?: string | undefined;
  /** The workspace's members' names, for its agents' owners. */
  names?: Names;
  /** The message in the chat's log to open it at, rather than at its latest. */
  at?: number | undefined;
}) {
  const [shown, setShown] = useState<"run" | "changes">("run");
  const access = useChatAccess(link);
  const reading = useLiveChat(link, chatRef, live, access.read);
  const commands = useChatCommands(link, chatRef, live, reading.answered);
  const changes = useChanges(link, chatRef, live, shown === "changes");
  const worktree = useWorktree(link, chatRef, live, mine, shown === "changes");
  const { chat, state } = reading.sync;
  // A message named while the changes show brings the transcript back.
  useEffect(() => {
    if (at !== undefined) {
      setShown("run");
    }
  }, [at]);
  const landed = landing(chat, at);
  const subscribed = state.kind === "live";
  // Each window that comes back moves `first` on, and asks for the next until the message is in.
  useEffect(() => {
    if (landed.kind === "older" && subscribed) {
      reading.more();
    }
  }, [landed.kind, chat?.first, subscribed]);
  const acts = mine && access.send;
  const stop = acts && chat !== undefined && working(chat) ? commands.stop : undefined;
  const why = blocked(live, chatRef, access.send);
  return (
    <div className="chat-screen">
      {(stop !== undefined || changes.askable || shown === "changes") && (
        <div className="toolbar">
          {shown === "run" ? (
            <>
              {stop !== undefined && <StopButton stopping={stop.stopping} problem={stop.problem} onStop={stop.stop} />}
              {changes.askable && (
                <button type="button" onClick={() => setShown("changes")}>
                  Changes
                </button>
              )}
            </>
          ) : (
            <button type="button" onClick={() => setShown("run")}>
              Back
            </button>
          )}
        </div>
      )}
      <div className="panes">
        <div className={shown === "run" ? "pane" : "pane away"}>
          <TranscriptView
            lines={chat === undefined ? [] : lines(chat, names)}
            older={chat !== undefined && state.kind === "live" && chat.first > 1 ? reading.more : undefined}
            answering={acts ? commands.answering : undefined}
            // A chat is on its way while the connection is made, then only from a server that serves chats.
            loading={chatRef !== undefined && chat === undefined && state.kind !== "failed" && (!live || access.read)}
            retry={state.kind === "failed" ? reading.open : undefined}
            away={away || shown !== "run"}
            landed={landed.kind === "line" ? landed.key : undefined}
          />
        </div>
        <ChatChanges
          changes={changes.changes}
          hidden={shown !== "changes"}
          mine={acts}
          send={commands.send}
          lates={commands.lates}
          blocked={why}
          bar={worktree.usable && <ReviewBar bar={worktree} title={title ?? ""} />}
        />
      </div>
      {mine && <Composer send={commands.send} blocked={why} hidden={shown !== "run"} lates={commands.lates} />}
    </div>
  );
}
