import { useEffect, useMemo, useState } from "react";

import type { AccountSignedIn, Code } from "../protocol.ts";
import { Api, type Fetch } from "./api.ts";
import { useAsk } from "./ask.ts";
import { Frame } from "./frame.tsx";
import { STORE, remember } from "./last-workspace.ts";
import { problemText } from "./problems.ts";
import "./shell.css";
import { SignIn } from "./sign-in.tsx";
import { Workspaces } from "./workspaces.tsx";

type Screen =
  | { name: "loading" }
  | { name: "failed"; code: Code }
  | { name: "signed out" }
  | { name: "signed in"; account: AccountSignedIn };

/** The app as the server serves it: sign-in, then the signed-in person's screens. */
export function Shell({ fetch }: { fetch: Fetch }) {
  const [screen, setScreen] = useState<Screen>({ name: "loading" });
  const api = useMemo(() => new Api(fetch, () => setScreen({ name: "signed out" })), [fetch]);
  const signingOut = useAsk();
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
  if (screen.name === "signed in") {
    const who = (
      <span className="who">
        {signingOut.problem !== undefined && <span role="alert">{signingOut.problem}</span>}
        <span className="muted">{screen.account.email}</span>
        <button
          type="button"
          disabled={signingOut.busy}
          onClick={() =>
            void signingOut.ask(
              () => api.signOut(),
              () => {
                remember(STORE, undefined);
                setScreen({ name: "signed out" });
              },
            )
          }
        >
          Sign out
        </button>
      </span>
    );
    return <Workspaces api={api} who={who} />;
  }
  return (
    <Frame>
      <Body screen={screen} api={api} signedIn={(account) => setScreen({ name: "signed in", account })} />
    </Frame>
  );
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
          <p role="alert">{problemText(screen.code)}</p>
          <button type="button" onClick={() => window.location.reload()}>
            Try again
          </button>
        </section>
      );
    case "signed out":
      return <SignIn api={api} signedIn={signedIn} />;
  }
}
