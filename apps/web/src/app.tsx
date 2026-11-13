import { useState } from "react";

import { Composer } from "./composer/composer.tsx";
import { blocked } from "./composer/prompt.ts";
import { type Status, statusText } from "./connection.ts";
import { type Changes, canAsk } from "./diff/changes.ts";
import { DiffView } from "./diff/diff-view.tsx";
import type { Place } from "./place.ts";
import type { App as Build } from "./protocol.ts";
import type { Transcript } from "./transcript.ts";
import { TranscriptView } from "./transcript-view.tsx";

type Shown = "run" | "changes" | "settings";

/** The chat's changes, and what opening them asks of the server. */
export type Review = { changes: Changes; openChanges: () => void };

/** The page: a header with Changes and Settings, over the run's transcript and the message box, its changes, or Settings. */
export function App({
  me,
  status,
  transcript,
  place,
  send,
  review,
}: {
  me: Build;
  status: Status;
  transcript: Transcript;
  place: Place | undefined;
  send: ((text: string) => void) | undefined;
  review: Review;
}) {
  const [shown, setShown] = useState<Shown>("run");
  return (
    <div className="app">
      <header>
        <span className="status">{statusText(status)}</span>
        {shown === "run" ? (
          <span className="actions">
            {place !== undefined && canAsk(status) && (
              <button
                type="button"
                onClick={() => {
                  review.openChanges();
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
      <main>{page(shown, me, status, transcript, review)}</main>
      <Composer send={send} blocked={blocked(status, place)} hidden={shown !== "run"} />
    </div>
  );
}

function page(shown: Shown, me: Build, status: Status, transcript: Transcript, { changes }: Review) {
  switch (shown) {
    case "run":
      return <TranscriptView rows={transcript.rows} writing={transcript.writing} />;
    case "changes":
      return <DiffView changes={changes} />;
    case "settings":
      return <Settings me={me} status={status} />;
  }
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
