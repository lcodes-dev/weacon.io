import type { End } from "../connection.ts";
import type { AccountSignedIn, AccountSignInFinish, AccountSignInStart, Code, SignInStarted } from "../protocol.ts";

/** The part of the browser's `fetch` this file uses. */
export type Fetch = (path: string, init: { method: string; headers?: Record<string, string>; body?: string }) => Promise<{
  status: number;
  json(): Promise<unknown>;
}>;

/** A server's answer: its body, or the code of why it refused or failed. */
export type Answer<T> = { ok: true; value: T } | { ok: false; code: Code };

/** Why there's no answer: the request never reached the server, or its answer wasn't one. */
export const UNREACHABLE: End = "connection.refused";
export const MALFORMED: Code = "answer.malformed";

/**
 * The server's API, on this page's own origin. The browser keeps the session cookie, which this
 * app never reads, and sends the `Origin` the server checks on each change.
 */
export class Api {
  #fetch: Fetch;

  constructor(fetch: Fetch) {
    this.#fetch = fetch;
  }

  /** Who is signed in, or `undefined` when nobody is. */
  async session(): Promise<Answer<AccountSignedIn | undefined>> {
    const answer = await this.#ask("GET", "/api/session", undefined, isAccount);
    return !answer.ok && answer.code === "sign_in.required" ? { ok: true, value: undefined } : answer;
  }

  /** Asks for a code to be sent to `email`; the answer is the same for addresses with no account. */
  start(email: string): Promise<Answer<SignInStarted>> {
    const body: AccountSignInStart = { email };
    return this.#ask("POST", "/api/sign-in", body, isStarted);
  }

  finish(body: AccountSignInFinish): Promise<Answer<AccountSignedIn>> {
    return this.#ask("POST", "/api/sign-in/finish", body, isAccount);
  }

  signOut(): Promise<Answer<undefined>> {
    return this.#ask("POST", "/api/sign-out", undefined, (value): value is undefined => value === undefined);
  }

  async #ask<T>(method: string, path: string, body: object | undefined, fits: (value: unknown) => value is T): Promise<Answer<T>> {
    const init = body === undefined ? { method } : { method, headers: { "content-type": "application/json" }, body: JSON.stringify(body) };
    const response = await this.#fetch(path, init).catch(() => undefined);
    if (response === undefined) {
      return { ok: false, code: UNREACHABLE };
    }
    const value = response.status === 204 ? undefined : await response.json().catch(() => undefined);
    if (response.status >= 200 && response.status < 300) {
      return fits(value) ? { ok: true, value } : { ok: false, code: MALFORMED };
    }
    return { ok: false, code: isError(value) ? value.code : MALFORMED };
  }
}

function fields(value: unknown, keys: string[]): boolean {
  return (
    typeof value === "object" &&
    value !== null &&
    keys.every((key) => typeof (value as Record<string, unknown>)[key] === "string")
  );
}

const isAccount = (value: unknown): value is AccountSignedIn => fields(value, ["account", "email"]);
const isStarted = (value: unknown): value is SignInStarted => fields(value, ["challenge"]);
const isError = (value: unknown): value is { code: Code } => fields(value, ["code"]);
