import { useState } from "react";

import { type Status, statusText } from "./connection.ts";
import type { App as Build } from "./protocol.ts";
import type { Transcript } from "./transcript.ts";
import { TranscriptView } from "./transcript-view.tsx";

/** The page: a header with Settings, over the run's transcript or Settings. */
export function App({ me, status, transcript }: { me: Build; status: Status; transcript: Transcript }) {
  const [settings, setSettings] = useState(false);
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
          <TranscriptView rows={transcript.rows} writing={transcript.writing} away={settings} />
        </div>
        {settings && (
          <div className="pane">
            <Settings me={me} status={status} />
          </div>
        )}
      </main>
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
