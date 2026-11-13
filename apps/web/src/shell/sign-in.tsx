import { useState } from "react";

import type { AccountSignedIn, ChallengeId } from "../protocol.ts";
import type { Answer, Api } from "./api.ts";
import { problemText } from "./problems.ts";

/**
 * Asks for an email, then the code sent to it. Every address gets the same answer, so the page
 * never says whether one has an account.
 */
export function SignIn({ api, signedIn }: { api: Api; signedIn(account: AccountSignedIn): void }) {
  const [email, setEmail] = useState("");
  const [code, setCode] = useState("");
  // The address the code was sent to, and the challenge to finish with.
  const [sent, setSent] = useState<{ email: string; challenge: ChallengeId }>();
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string>();
  const ask = async <T,>(request: Promise<Answer<T>>, then: (value: T) => void) => {
    setBusy(true);
    setProblem(undefined);
    const answer = await request;
    setBusy(false);
    if (answer.ok) {
      then(answer.value);
    } else {
      setProblem(problemText(answer.code));
    }
  };
  const send = () => {
    const to = email.trim();
    void ask(api.start(to), ({ challenge }) => {
      setCode("");
      setSent({ email: to, challenge });
    });
  };
  return (
    <section className="page sign-in">
      <h1>Sign in</h1>
      {sent === undefined ? (
        <form
          onSubmit={(event) => {
            event.preventDefault();
            send();
          }}
        >
          <label>
            Email
            <input type="email" autoComplete="email" required value={email} onChange={(event) => setEmail(event.target.value)} />
          </label>
          <button type="submit" disabled={busy}>
            Send code
          </button>
        </form>
      ) : (
        <form
          onSubmit={(event) => {
            event.preventDefault();
            void ask(api.finish({ ...sent, code: code.trim() }), signedIn);
          }}
        >
          <p>If {sent.email} can sign in here, we sent it a code.</p>
          <label>
            Code
            <input
              inputMode="numeric"
              autoComplete="one-time-code"
              required
              value={code}
              onChange={(event) => setCode(event.target.value)}
            />
          </label>
          <button type="submit" disabled={busy}>
            Sign in
          </button>
          <button
            type="button"
            disabled={busy}
            onClick={() => {
              setSent(undefined);
              setProblem(undefined);
            }}
          >
            Use another email
          </button>
        </form>
      )}
      {problem !== undefined && <p role="alert">{problem}</p>}
    </section>
  );
}
