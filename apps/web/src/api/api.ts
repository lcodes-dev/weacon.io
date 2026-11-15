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
  DocContent,
  DocHistory,
  DocId,
  DocList,
  DocRecord,
  DocRevise,
  MachineId,
  MachineList,
  MachineSummary,
  Mark,
  MemberList,
  PairConfirm,
  PairLookup,
  PairPending,
  ProposalDecision,
  ProposalId,
  ProposalList,
  ProposalOutcome,
  ProposalVerdict,
  SearchAnswer,
  SearchQuery,
  SignInStarted,
  TaskChange,
  TaskCreate,
  TaskEditBody,
  TaskId,
  TaskList,
  TaskRecord,
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

  /** What in `workspace` holds every word of `query`'s text, best first. */
  search(workspace: WorkspaceId, query: SearchQuery): Promise<Answer<SearchAnswer>> {
    return this.#ask("POST", `${base(workspace)}/search`, query, isSearchAnswer);
  }

  /** Who is or was a member of `workspace`, with the name each is shown by. */
  members(workspace: WorkspaceId): Promise<Answer<MemberList>> {
    return this.#ask("GET", `${base(workspace)}/members`, undefined, isMembers);
  }

  /** The tasks of `workspace`, in their order. */
  tasks(workspace: WorkspaceId): Promise<Answer<TaskList>> {
    return this.#ask("GET", `${base(workspace)}/tasks`, undefined, isTasks);
  }

  /** Makes a task, to do, placed last. */
  createTask(workspace: WorkspaceId, title: string): Promise<Answer<TaskRecord>> {
    const body: TaskCreate = { title };
    return this.#ask("POST", `${base(workspace)}/tasks`, body, isTask);
  }

  /** Makes one edit to a task read at `version`; one changed since is refused as `task.stale`. */
  changeTask(workspace: WorkspaceId, task: TaskId, version: number, edit: TaskEditBody): Promise<Answer<TaskRecord>> {
    const body: TaskChange = { version, edit };
    return this.#ask("POST", `${base(workspace)}/tasks/${encodeURIComponent(task)}`, body, isTask);
  }

  /** The docs of `workspace`, without their bodies. */
  docs(workspace: WorkspaceId): Promise<Answer<DocList>> {
    return this.#ask("GET", `${base(workspace)}/docs`, undefined, isDocs);
  }

  doc(workspace: WorkspaceId, doc: DocId): Promise<Answer<DocRecord>> {
    return this.#ask("GET", docPath(workspace, doc), undefined, isDoc);
  }

  /** Who made the doc's revisions, newest first. */
  docHistory(workspace: WorkspaceId, doc: DocId): Promise<Answer<DocHistory>> {
    return this.#ask("GET", `${docPath(workspace, doc)}/revisions`, undefined, isHistory);
  }

  /** The doc as it was at `version`. */
  docRevision(workspace: WorkspaceId, doc: DocId, version: number): Promise<Answer<DocRecord>> {
    return this.#ask("GET", `${docPath(workspace, doc)}/revisions/${version}`, undefined, isDoc);
  }

  createDoc(workspace: WorkspaceId, content: DocContent): Promise<Answer<DocRecord>> {
    return this.#ask("POST", `${base(workspace)}/docs`, content, isDoc);
  }

  /** Replaces the text of a doc read at `version`; one revised since is refused as `doc.stale`. */
  reviseDoc(workspace: WorkspaceId, doc: DocId, version: number, content: DocContent): Promise<Answer<DocRecord>> {
    const body: DocRevise = { version, content };
    return this.#ask("POST", docPath(workspace, doc), body, isDoc);
  }

  /** The newest proposals of `workspace`, newest first. */
  proposals(workspace: WorkspaceId): Promise<Answer<ProposalList>> {
    return this.#ask("GET", `${base(workspace)}/proposals`, undefined, isProposals);
  }

  /** Accepts or rejects an open proposal. */
  decide(workspace: WorkspaceId, proposal: ProposalId, verdict: ProposalVerdict): Promise<Answer<ProposalOutcome>> {
    const body: ProposalDecision = { verdict };
    return this.#ask("POST", `${base(workspace)}/proposals/${encodeURIComponent(proposal)}`, body, isOutcome);
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

const base = (workspace: WorkspaceId) => `/api/workspaces/${encodeURIComponent(workspace)}`;
const docPath = (workspace: WorkspaceId, doc: DocId) => `${base(workspace)}/docs/${encodeURIComponent(doc)}`;

// What a body that doesn't parse reads as, which no answer's shape fits.
const NOT_JSON = Symbol("not JSON");

const isObject = (value: unknown): value is Record<string, unknown> => typeof value === "object" && value !== null;

function fields(value: unknown, keys: string[]): boolean {
  return isObject(value) && keys.every((key) => typeof value[key] === "string");
}

const isAccount = (value: unknown): value is AccountSignedIn => fields(value, ["account", "email"]);
const isStarted = (value: unknown): value is SignInStarted => fields(value, ["challenge"]);
const isError = (value: unknown): value is { code: Code } => fields(value, ["code"]);
const isWorkspace = (value: unknown): value is WorkspaceSummary =>
  fields(value, ["id", "name", "role"]) && isId((value as WorkspaceSummary).id);
/** A guard for an object whose `key` holds a list of items that `item` accepts. */
function listOf<T>(key: string, item: (value: unknown) => boolean): (value: unknown) => value is T {
  return (value): value is T => {
    const items = isObject(value) ? value[key] : undefined;
    return Array.isArray(items) && items.every(item);
  };
}

const isList = listOf<WorkspaceList>("workspaces", isWorkspace);
const isChat = (value: unknown): value is ChatSummary =>
  fields(value, ["id", "machine", "owner", "state", "title"]) &&
  isId((value as ChatSummary).id) &&
  Number.isSafeInteger((value as ChatSummary).updated_at) &&
  ["undefined", "string"].includes(typeof (value as ChatSummary).run);
const isChats = listOf<ChatList>("chats", isChat);
const isMark = (value: unknown): boolean =>
  typeof value === "object" && value !== null && Number.isSafeInteger((value as Mark).start) && Number.isSafeInteger((value as Mark).end);
const isSnippet = (value: unknown): boolean => fields(value, ["text"]) && listOf("marks", isMark)(value);
// A hit of a kind this build doesn't know only needs its kind; the palette leaves it out.
const isHit = (value: unknown): boolean => {
  if (!fields(value, ["kind"])) {
    return false;
  }
  const hit = value as { kind: string; snippet: unknown; doc: string; chat: string };
  const known = fields(value, ["title"]) && isSnippet(hit.snippet);
  // A doc's or a chat's id goes into the address, so it must be one.
  switch (hit.kind) {
    case "task":
      return known;
    case "doc":
      return known && fields(value, ["doc"]) && isId(hit.doc);
    case "chat":
      return known && fields(value, ["chat"]) && isId(hit.chat);
    default:
      return true;
  }
};
const isSearchAnswer = listOf<SearchAnswer>("hits", isHit);
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
const versioned = (value: unknown, keys: string[]): boolean =>
  fields(value, ["id", ...keys]) && isId((value as { id: string }).id) && Number.isSafeInteger((value as { version: unknown }).version);
const isMembers = listOf<MemberList>("members", (value) => fields(value, ["id", "name"]));
const isTask = (value: unknown): value is TaskRecord => versioned(value, ["title", "status", "order"]);
const isTasks = listOf<TaskList>("tasks", isTask);
const isDocs = listOf<DocList>("docs", (value) => versioned(value, ["title"]));
const isDoc = (value: unknown): value is DocRecord => versioned(value, []) && fields((value as DocRecord).content, ["title", "body"]);
const isRevision = (value: unknown): boolean =>
  fields(value, ["accepted_by"]) && isObject(value) && isObject(value["author"]) && Number.isSafeInteger(value["version"]);
const isHistory = listOf<DocHistory>("revisions", isRevision);
const isProposal = (value: unknown): boolean =>
  fields(value, ["id", "state"]) && isObject(value) && isObject(value["about"]) && isObject(value["author"]) && Number.isSafeInteger(value["at"]);
const isProposals = listOf<ProposalList>("proposals", isProposal);
const isHunk = (value: unknown): boolean =>
  isObject(value) && (value["kind"] === "agreed" ? fields(value, ["text"]) : value["kind"] === "conflict" ? fields(value, ["base", "current", "proposed"]) : fields(value, ["kind"]));
// Each outcome this app reads has its fields; one of a newer kind passes, to be shown as such.
const isOutcome = (value: unknown): value is ProposalOutcome => {
  if (!fields(value, ["outcome"]) || !isObject(value)) {
    return false;
  }
  switch (value["outcome"]) {
    case "accepted":
      return Number.isSafeInteger(value["version"]);
    case "doc_conflict":
      return typeof value["title"] === "boolean" && typeof value["anchors"] === "boolean" && Array.isArray(value["hunks"]) && value["hunks"].every(isHunk);
    case "task_conflict":
      return Array.isArray(value["edits"]) && value["edits"].every((edit) => fields(edit, ["kind"]));
    default:
      return true;
  }
};
