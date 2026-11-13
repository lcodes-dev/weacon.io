import { useState } from "react";

import { type Status, statusText } from "./connection.ts";
import type { App as Build } from "./protocol.ts";

/** The page: a header with Settings, over the page or Settings. */
export function App({ me, status }: { me: Build; status: Status }) {
  const [settings, setSettings] = useState(false);
  return (
    <div className="app">
      <header>
        <span className="status">{statusText(status)}</span>
        <button type="button" onClick={() => setSettings(!settings)}>
          {settings ? "Back" : "Settings"}
        </button>
      </header>
      <main>{settings ? <Settings me={me} status={status} /> : <p className="muted">No run yet.</p>}</main>
    </div>
  );
}

function Settings({ me, status }: { me: Build; status: Status }) {
  return (
    <section>
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
