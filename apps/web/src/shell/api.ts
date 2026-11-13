import type { End } from "../connection.ts";
import { isId } from "../place.ts";
import type {
  AccountSignedIn,
  AccountSignInFinish,
  AccountSignInStart,
  Code,
  SignInStarted,
  WorkspaceCreate,
  WorkspaceList,
  WorkspaceSummary,
} from "../protocol.ts";

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
const SIGNED_OUT: Code = "sign_in.required";

/**
 * The server's API, on this page's own origin. The browser keeps the session cookie, which this
 * app never reads, and sends the `Origin` the server checks on each change.
 */
export class Api {
  #fetch: Fetch;
  #signedOut: () => void;

  /** `signedOut` is told whenever the server answers that the session is over. */
  constructor(fetch: Fetch, signedOut: () => void) {
    this.#fetch = fetch;
    this.#signedOut = signedOut;
  }

  /** Who is signed in, or `undefined` when nobody is. */
  async session(): Promise<Answer<AccountSignedIn | undefined>> {
    const answer = await this.#ask("GET", "/api/session", undefined, isAccount);
    return !answer.ok && answer.code === SIGNED_OUT ? { ok: true, value: undefined } : answer;
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

  workspaces(): Promise<Answer<WorkspaceList>> {
    return this.#ask("GET", "/api/workspaces", undefined, isList);
  }

  /** Makes a workspace the signed-in account owns. */
  create(name: string): Promise<Answer<WorkspaceSummary>> {
    const body: WorkspaceCreate = { name };
    return this.#ask("POST", "/api/workspaces", body, isWorkspace);
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
    const code = isError(value) ? value.code : MALFORMED;
    if (code === SIGNED_OUT) {
      this.#signedOut();
    }
    return { ok: false, code };
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
const isWorkspace = (value: unknown): value is WorkspaceSummary =>
  fields(value, ["id", "name", "role"]) && isId((value as WorkspaceSummary).id);
const isList = (value: unknown): value is WorkspaceList =>
  typeof value === "object" &&
  value !== null &&
  "workspaces" in value &&
  Array.isArray(value.workspaces) &&
  value.workspaces.every(isWorkspace);
