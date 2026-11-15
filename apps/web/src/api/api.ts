import type { End } from "../connection.ts";
import { isId } from "../ids.ts";
import { isMachineKey } from "../machines/fingerprint.ts";
import type {
  AccountSignedIn,
  AccountSignInFinish,
  AccountSignInStart,
  ChatList,
  ChatSummary,
  Code,
  MachineId,
  MachineList,
  MachineSummary,
  PairConfirm,
  PairLookup,
  PairPending,
  SignInStarted,
  WorkspaceCreate,
  WorkspaceId,
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
  #signIns = 0;

  /** `signedOut` is told when the server answers that the current session is over. */
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
  startSignIn(email: string): Promise<Answer<SignInStarted>> {
    const body: AccountSignInStart = { email };
    return this.#ask("POST", "/api/sign-in", body, isStarted);
  }

  async finishSignIn(body: AccountSignInFinish): Promise<Answer<AccountSignedIn>> {
    const answer = await this.#ask("POST", "/api/sign-in/finish", body, isAccount);
    if (answer.ok) {
      this.#signIns++;
    }
    return answer;
  }

  signOut(): Promise<Answer<undefined>> {
    return this.#ask("POST", "/api/sign-out", undefined, isEmpty);
  }

  workspaces(): Promise<Answer<WorkspaceList>> {
    return this.#ask("GET", "/api/workspaces", undefined, isList);
  }

  /** Makes a workspace the signed-in account owns. */
  createWorkspace(name: string): Promise<Answer<WorkspaceSummary>> {
    const body: WorkspaceCreate = { name };
    return this.#ask("POST", "/api/workspaces", body, isWorkspace);
  }

  /** The chats of `workspace` the signed-in member can see. */
  chats(workspace: WorkspaceId): Promise<Answer<ChatList>> {
    return this.#ask("GET", `/api/workspaces/${workspace}/chats`, undefined, isChats);
  }

  /** The machines paired with the signed-in account, oldest first. */
  machines(): Promise<Answer<MachineList>> {
    return this.#ask("GET", "/api/machines", undefined, isMachineList);
  }

  /** The machine waiting to be paired under `code`, as the person typed it: the server reads it loosely. */
  lookUpPairing(code: string): Promise<Answer<PairPending>> {
    const body: PairLookup = { code };
    return this.#ask("POST", "/api/machines/pair/lookup", body, isPending);
  }

  /** Pairs the machine `lookUpPairing` found with the signed-in account. */
  confirmPairing(body: PairConfirm): Promise<Answer<MachineSummary>> {
    return this.#ask("POST", "/api/machines/pair/confirm", body, isMachine);
  }

  /** Unpairs `machine`; one already unpaired elsewhere is as gone as one unpaired here. */
  async unpair(machine: MachineId): Promise<Answer<undefined>> {
    const answer = await this.#ask("DELETE", `/api/machines/${encodeURIComponent(machine)}`, undefined, isEmpty);
    return !answer.ok && answer.code === "machine.not_found" ? { ok: true, value: undefined } : answer;
  }

  async #ask<T>(method: string, path: string, body: object | undefined, fits: (value: unknown) => value is T): Promise<Answer<T>> {
    const signIns = this.#signIns;
    const init = body === undefined ? { method } : { method, headers: { "content-type": "application/json" }, body: JSON.stringify(body) };
    const response = await this.#fetch(path, init).catch(() => undefined);
    if (response === undefined) {
      return { ok: false, code: UNREACHABLE };
    }
    const value = response.status === 204 ? undefined : await response.json().catch(() => NOT_JSON);
    if (response.status >= 200 && response.status < 300) {
      return fits(value) ? { ok: true, value } : { ok: false, code: MALFORMED };
    }
    const code = isError(value) ? value.code : MALFORMED;
    if (code === SIGNED_OUT && signIns === this.#signIns) {
      this.#signedOut();
    }
    return { ok: false, code };
  }
}

// What a body that doesn't parse reads as, which no answer's shape fits.
const NOT_JSON = Symbol("not JSON");

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
/** A guard for an object whose `key` holds a list of items that `item` accepts. */
function listOf<T>(key: string, item: (value: unknown) => boolean): (value: unknown) => value is T {
  return (value): value is T => {
    const items = typeof value === "object" && value !== null ? (value as Record<string, unknown>)[key] : undefined;
    return Array.isArray(items) && items.every(item);
  };
}

const isList = listOf<WorkspaceList>("workspaces", isWorkspace);
const isChat = (value: unknown): value is ChatSummary =>
  fields(value, ["id", "machine", "owner", "state", "title"]) &&
  isId((value as ChatSummary).id) &&
  Number.isSafeInteger((value as ChatSummary).updated_at);
const isChats = listOf<ChatList>("chats", isChat);
const isEmpty = (value: unknown): value is undefined => value === undefined;
const isPending = (value: unknown): value is PairPending =>
  fields(value, ["pairing", "name", "os", "key"]) &&
  isId((value as PairPending).pairing) &&
  isMachineKey((value as PairPending).key);
const isMachine = (value: unknown): value is MachineSummary =>
  fields(value, ["id", "name", "os", "key"]) &&
  isId((value as MachineSummary).id) &&
  isMachineKey((value as MachineSummary).key) &&
  typeof (value as MachineSummary).paired_at === "number";
const isMachineList = listOf<MachineList>("machines", isMachine);
