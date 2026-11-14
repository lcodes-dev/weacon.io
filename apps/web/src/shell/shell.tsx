import { useCallback, useEffect, useMemo, useState } from "react";

import { Api, type Fetch } from "../api/api.ts";
import { useAsk } from "../api/ask.ts";
import { Problem } from "../api/problem.tsx";
import { problemText } from "../api/problems.ts";
import type { Later } from "../connection.ts";
import type { Log } from "../log.ts";
import { Machines } from "../machines/machines.tsx";
import { Pins, browserStore } from "../machines/pins.ts";
import { ME } from "../me.ts";
import type { AccountSignedIn, Code } from "../protocol.ts";
import { Settings } from "../settings/settings.tsx";
import { Frame } from "./frame.tsx";
import { STORE, remember } from "./last-workspace.ts";
import "./shell.css";
import { SignIn } from "./sign-in.tsx";
import { Workspaces } from "./workspaces.tsx";

type Screen =
  | { name: "loading" }
  | { name: "failed"; code: Code }
  | { name: "signed out" }
  | { name: "signed in"; account: AccountSignedIn };

const later: Later = (run, ms) => setTimeout(run, ms);

// Keys pinned in this browser, opened on first use.
const PINS = new Pins(browserStore());

/** The app as the server serves it: sign-in, then the signed-in person's screens. */
export function Shell({ fetch, log }: { fetch: Fetch; log: Log }) {
  const [screen, setScreen] = useState<Screen>({ name: "loading" });
  // However a session ends, the workspace it last opened is forgotten, so the next person starts afresh.
  const signedOut = useCallback(() => {
    remember(STORE, undefined);
    window.history.replaceState(null, "", window.location.pathname);
    setScreen({ name: "signed out" });
  }, []);
  const api = useMemo(() => new Api(fetch, signedOut), [fetch, signedOut]);
  useEffect(() => {
    // An answer that comes after the page moved on must not take it back.
    let current = true;
    void api.session().then((answer) => {
      if (!current) {
        return;
      }
      if (!answer.ok) {
        setScreen({ name: "failed", code: answer.code });
      } else if (answer.value === undefined) {
        signedOut();
      } else {
        setScreen({ name: "signed in", account: answer.value });
      }
    });
    return () => {
      current = false;
    };
  }, [api, signedOut]);
  if (screen.name === "signed in") {
    return <SignedIn api={api} log={log} account={screen.account} signedOut={signedOut} />;
  }
  return (
    <Frame>
      <Body screen={screen} api={api} signedIn={(account) => setScreen({ name: "signed in", account })} />
    </Frame>
  );
}

// The signed-in person's workspaces, or their settings, with who they are and Sign out.
function SignedIn({ api, log, account, signedOut }: { api: Api; log: Log; account: AccountSignedIn; signedOut(): void }) {
  const [settings, setSettings] = useState(false);
  const signingOut = useAsk();
  const signOut = () => void signingOut.ask(() => api.signOut(), signedOut);
  const who = (
    <span className="who">
      {signingOut.problem !== undefined && <span role="alert">{signingOut.problem}</span>}
      {!settings && (
        <>
          <span className="muted">{account.email}</span>
          <button type="button" disabled={signingOut.busy} onClick={signOut}>
            Sign out
          </button>
        </>
      )}
      <button
        type="button"
        onClick={() => {
          signingOut.clear();
          setSettings(!settings);
        }}
      >
        {settings ? "Back" : "Settings"}
      </button>
    </span>
  );
  const page = settings ? (
    <Settings me={ME} account={{ email: account.email, signOut, signingOut: signingOut.busy }}>
      <Machines api={api} pins={PINS} later={later} />
    </Settings>
  ) : undefined;
  return <Workspaces api={api} log={log} who={who} page={page} />;
}

function Body({
  screen,
  api,
  signedIn,
}: {
  screen: Exclude<Screen, { name: "signed in" }>;
  api: Api;
  signedIn(account: AccountSignedIn): void;
}) {
  switch (screen.name) {
    case "loading":
      return <p className="page muted">Loading</p>;
    case "failed":
      return (
        <section className="page">
          <Problem text={problemText(screen.code)} retry={() => window.location.reload()} />
        </section>
      );
    case "signed out":
      return <SignIn api={api} signedIn={signedIn} />;
  }
}
