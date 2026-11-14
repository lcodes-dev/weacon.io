import type { App as Build } from "../protocol.ts";

/** The signed-in account, and signing it out. */
export type Account = { email: string; signOut(): void; signingOut: boolean };

/** Settings: the account, when someone is signed in, and which builds are running. */
export function Settings({ me, server, account }: { me: Build; server?: Build | undefined; account?: Account }) {
  return (
    <section className="page">
      <h1>Settings</h1>
      {account !== undefined && (
        <>
          <h2>Account</h2>
          <dl>
            <dt>Email</dt>
            <dd>{account.email}</dd>
          </dl>
          <button type="button" disabled={account.signingOut} onClick={account.signOut}>
            Sign out
          </button>
        </>
      )}
      <h2>About</h2>
      <dl>
        <dt>Version</dt>
        <dd>{me.version}</dd>
        <dt>Commit</dt>
        <dd>{me.commit}</dd>
        {server !== undefined && (
          <>
            <dt>Server</dt>
            <dd>
              {server.version} ({server.commit})
            </dd>
          </>
        )}
      </dl>
    </section>
  );
}
