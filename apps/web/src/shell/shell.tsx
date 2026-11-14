import { useEffect, useState } from "react";

import type { AccountSignedIn, App as Build, Code } from "../protocol.ts";
import { Settings } from "../settings/settings.tsx";
import type { Api } from "./api.ts";
import { problemText } from "./problems.ts";
import "./shell.css";
import { SignIn } from "./sign-in.tsx";

type Screen =
  | { name: "loading" }
  | { name: "failed"; code: Code }
  | { name: "signed out" }
  | { name: "signed in"; account: AccountSignedIn; settings: boolean };

/** The app as the server serves it: sign-in, then the signed-in person's screens. */
export function Shell({ api, me }: { api: Api; me: Build }) {
  const [screen, setScreen] = useState<Screen>({ name: "loading" });
  const [problem, setProblem] = useState<string>();
  const [signingOut, setSigningOut] = useState(false);
  useEffect(() => {
    // An answer that comes after the page moved on must not take it back.
    let current = true;
    void api.session().then((answer) => {
      if (current) {
        setScreen(
          !answer.ok
            ? { name: "failed", code: answer.code }
            : answer.value === undefined
              ? { name: "signed out" }
              : { name: "signed in", account: answer.value, settings: false },
        );
      }
    });
    return () => {
      current = false;
    };
  }, [api]);
  const signOut = async () => {
    setProblem(undefined);
    setSigningOut(true);
    const answer = await api.signOut();
    setSigningOut(false);
    if (answer.ok) {
      setScreen({ name: "signed out" });
    } else {
      setProblem(problemText(answer.code));
    }
  };
  const toggleSettings = () => {
    setProblem(undefined);
    setScreen((before) => (before.name === "signed in" ? { ...before, settings: !before.settings } : before));
  };
  return (
    <div className="app">
      <header>
        <span>Weacon</span>
        {screen.name === "signed in" && (
          <button type="button" onClick={toggleSettings}>
            {screen.settings ? "Back" : "Settings"}
          </button>
        )}
      </header>
      {problem !== undefined && <p role="alert">{problem}</p>}
      <main>
        {screen.name === "signed in" && screen.settings ? (
          <Settings
            me={me}
            account={{ email: screen.account.email, signOut: () => void signOut(), signingOut }}
          />
        ) : (
          <Body screen={screen} api={api} signedIn={(account) => setScreen({ name: "signed in", account, settings: false })} />
        )}
      </main>
    </div>
  );
}

function Body({ screen, api, signedIn }: { screen: Screen; api: Api; signedIn(account: AccountSignedIn): void }) {
  switch (screen.name) {
    case "loading":
      return <p className="page muted">Loading</p>;
    case "failed":
      return (
        <section className="page">
          <p role="alert">{problemText(screen.code)}</p>
          <button type="button" onClick={() => window.location.reload()}>
            Try again
          </button>
        </section>
      );
    case "signed out":
      return <SignIn api={api} signedIn={signedIn} />;
    case "signed in":
      return <p className="page">You're signed in.</p>;
  }
}
