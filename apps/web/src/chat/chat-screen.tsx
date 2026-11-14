import { type ReactNode, useState } from "react";

import type { ChatRef } from "../chat-ref.ts";
import { Composer } from "../composer/composer.tsx";
import { blocked } from "../composer/prompt.ts";
import { ChatChanges } from "../diff/chat-changes.tsx";
import { useChanges } from "../diff/use-changes.ts";
import type { Link } from "../shell/workspace-socket.ts";
import { useChatAccess } from "./access.ts";
import { working } from "./chat.ts";
import { useChatCommands, useLiveChat } from "./chat-hooks.ts";
import { lines } from "./lines.ts";
import { StopButton } from "./stop-button.tsx";
import { TranscriptView } from "./transcript-view.tsx";
import "./chat.css";

// No message carries members' names, so labels leave them out.
const NO_MEMBERS = new Map<string, string>();

/**
 * The chat at `chatRef` over its workspace's `link`: its transcript, and its changes with Changes,
 * or a `page` such as Settings in their place while the transcript keeps its place beneath. Its
 * owner (`mine`) also gets the message box, Stop and the permission prompts' answers, where the
 * server takes commands.
 */
export function ChatScreen({
  link,
  live,
  chatRef,
  mine,
  page,
}: {
  link: Link;
  /** Whether the link's connection is up. */
  live: boolean;
  chatRef: ChatRef | undefined;
  mine: boolean;
  page?: ReactNode;
}) {
  const [shown, setShown] = useState<"run" | "changes">("run");
  const access = useChatAccess(link);
  const reading = useLiveChat(link, chatRef, live, access.read);
  const commands = useChatCommands(link, chatRef, live, reading.answered);
  const changes = useChanges(link, chatRef, live);
  const { chat, state } = reading.sync;
  const acts = mine && access.send;
  const stop = acts && chat !== undefined && working(chat) ? commands.stop : undefined;
  const why = blocked(live, chatRef, access.send);
  const covered = page !== undefined;
  return (
    <div className="chat-screen">
      {!covered && (stop !== undefined || changes.askable || shown === "changes") && (
        <div className="toolbar">
          {shown === "run" ? (
            <>
              {stop !== undefined && <StopButton stopping={stop.stopping} problem={stop.problem} onStop={stop.stop} />}
              {changes.askable && (
                <button
                  type="button"
                  onClick={() => {
                    changes.open();
                    setShown("changes");
                  }}
                >
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
      <main>
        <div className={shown === "run" ? "pane" : "pane away"}>
          <TranscriptView
            lines={chat === undefined ? [] : lines(chat, NO_MEMBERS)}
            older={chat !== undefined && state.kind === "live" && chat.first > 1 ? reading.more : undefined}
            // Only the chat's owner answers its prompts.
            answering={acts ? commands.answering : undefined}
            // A chat is on its way while the connection is made, then only from a server that serves chats.
            loading={chatRef !== undefined && chat === undefined && state.kind !== "failed" && (!live || access.read)}
            retry={state.kind === "failed" ? reading.open : undefined}
            away={covered || shown !== "run"}
          />
        </div>
        {covered && <div className="pane">{page}</div>}
        <ChatChanges
          changes={changes.changes}
          hidden={covered || shown !== "changes"}
          mine={acts}
          send={commands.send}
          late={commands.late}
          blocked={why}
        />
      </main>
      {mine && <Composer send={commands.send} blocked={why} hidden={covered || shown !== "run"} late={commands.late} />}
    </div>
  );
}
