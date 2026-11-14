import { useState } from "react";

import { Composer } from "./composer/composer.tsx";
import { type Send, blocked } from "./composer/prompt.ts";
import { type Status, statusText } from "./connection.ts";
import { type Changes, canAsk } from "./diff/changes.ts";
import type { Comment } from "./diff/comments.ts";
import { DiffView } from "./diff/diff-view.tsx";
import type { ChatRef } from "./chat-ref.ts";
import type { App as Build } from "./protocol.ts";
import { Settings } from "./settings/settings.tsx";
import type { Transcript } from "./transcript/transcript.ts";
import { TranscriptView } from "./transcript/transcript-view.tsx";

type Shown = "run" | "changes" | "settings";

/** The page: a header with Changes and Settings, over the run's transcript and the message box, its changes, or Settings. */
export function App({
  me,
  status,
  transcript,
  chatRef,
  send,
  changes,
  openChanges,
}: {
  me: Build;
  status: Status;
  transcript: Transcript;
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
          <TranscriptView rows={transcript.rows} writing={transcript.writing} away={shown !== "run"} />
        </div>
        {shown === "changes" && (
          <div className="pane">
            <DiffView changes={changes} comments={comments} setComments={setComments} />
          </div>
        )}
        {shown === "settings" && (
          <div className="pane">
            <Settings me={me} server={status.state === "live" ? status.server : undefined} />
          </div>
        )}
      </main>
      <Composer send={send} blocked={blocked(status, chatRef)} hidden={shown !== "run"} />
    </div>
  );
}
