import { useEffect, useState } from "react";

import type { AccountSignedIn, Code } from "../protocol.ts";
import type { Api } from "./api.ts";
import { problemText } from "./problems.ts";
import "./shell.css";
import { SignIn } from "./sign-in.tsx";

type Screen =
  | { name: "loading" }
  | { name: "failed"; code: Code }
  | { name: "signed out" }
  | { name: "signed in"; account: AccountSignedIn };

/** The app as the server serves it: sign-in, then the signed-in person's screens. */
export function Shell({ api }: { api: Api }) {
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
              : { name: "signed in", account: answer.value },
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
  return (
    <div className="app">
      <header>
        <span>Weacon</span>
        {screen.name === "signed in" && (
          <span className="who">
            <span className="muted">{screen.account.email}</span>
            <button type="button" disabled={signingOut} onClick={() => void signOut()}>
              Sign out
            </button>
          </span>
        )}
      </header>
      {problem !== undefined && <p role="alert">{problem}</p>}
      <main>
        <Body screen={screen} api={api} signedIn={(account) => setScreen({ name: "signed in", account })} />
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
