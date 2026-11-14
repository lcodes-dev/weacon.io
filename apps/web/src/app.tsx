import { useState } from "react";

import { Composer } from "./composer/composer.tsx";
import { type Send, blocked } from "./composer/prompt.ts";
import { type Status, statusText } from "./connection.ts";
import { type Changes, canAsk } from "./diff/changes.ts";
import type { Comment } from "./diff/comments.ts";
import { DiffView } from "./diff/diff-view.tsx";
import type { ChatRef } from "./chat-ref.ts";
import type { App as Build } from "./protocol.ts";
import type { Line } from "./chat/lines.ts";
import { StopButton } from "./chat/parts.tsx";
import { type Answering, TranscriptView } from "./transcript/transcript-view.tsx";

type Shown = "run" | "changes" | "settings";

/** What the chat shows, and what a person can do with it. */
export type ChatView = {
  lines: readonly Line[];
  older: (() => void) | undefined;
  answering: Answering;
  /** Stops the agent's turn, while one is running. */
  stop: { stopping: boolean; stop: () => void } | undefined;
};

/** The page: a header with Stop, Changes and Settings, over the chat's transcript and the message box, its changes, or Settings. */
export function App({
  me,
  status,
  chat,
  chatRef,
  send,
  changes,
  openChanges,
}: {
  me: Build;
  status: Status;
  chat: ChatView;
  chatRef: ChatRef | undefined;
  send: Send;
  changes: Changes;
  /** Asks the server for the chat's changes, when they need asking. */
  openChanges: () => void;
}) {
  const [shown, setShown] = useState<Shown>("run");
  const [comments, setComments] = useState<readonly Comment[]>([]);
  return (
    <div className="app">
      <header>
        <span className="status">{statusText(status)}</span>
        {shown === "run" ? (
          <span className="actions">
            {chat.stop !== undefined && <StopButton stopping={chat.stop.stopping} onStop={chat.stop.stop} />}
            {chatRef !== undefined && canAsk(status) && (
              <button
                type="button"
                onClick={() => {
                  openChanges();
                  setShown("changes");
                }}
              >
                Changes
              </button>
            )}
            <button type="button" onClick={() => setShown("settings")}>
              Settings
            </button>
          </span>
        ) : (
          <button type="button" onClick={() => setShown("run")}>
            Back
          </button>
        )}
      </header>
      <main>
        <div className={shown === "run" ? "pane" : "pane away"}>
          <TranscriptView lines={chat.lines} older={chat.older} answering={chat.answering} away={shown !== "run"} />
        </div>
        {shown === "changes" && (
          <div className="pane">
            <DiffView changes={changes} comments={comments} setComments={setComments} />
          </div>
        )}
        {shown === "settings" && (
          <div className="pane">
            <Settings me={me} status={status} />
          </div>
        )}
      </main>
      <Composer send={send} blocked={blocked(status, chatRef)} hidden={shown !== "run"} />
    </div>
  );
}

function Settings({ me, status }: { me: Build; status: Status }) {
  return (
    <section className="page">
      <h1>Settings</h1>
      <h2>About</h2>
      <dl>
        <dt>Version</dt>
        <dd>{me.version}</dd>
        <dt>Commit</dt>
        <dd>{me.commit}</dd>
        {status.state === "live" && (
          <>
            <dt>Server</dt>
            <dd>
              {status.server.version} ({status.server.commit})
            </dd>
          </>
        )}
      </dl>
    </section>
  );
}
