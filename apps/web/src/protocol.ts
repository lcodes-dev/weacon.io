// Generated from crates/protocol/schema.json by tooling/ts/generate-types.ts. Don't edit it.

/**
 * One message on the wire. A reply carries the `id` of the request it answers.
 */
export type Envelope = {
  id: number;
} & (
  | {
    type: "hello";
  } & Hello
  | {
    type: "welcome";
  } & Welcome
  | {
    type: "error";
  } & ErrorBody
  | {
    type: "receipt";
  } & Receipt
  | {
    type: "pick";
  } & Pick
  | {
    type: "run";
  } & RunUpdate
  | {
    type: "workspace";
  } & WorkspaceUpdate
  | {
    type: "log";
  } & ClientLog
  | {
    type: "chat_command";
  } & ChatCommand
  | {
    type: "chat";
  } & ChatUpdate
  | {
    type: "chat_query";
  } & ChatQuery
  | {
    type: "chat_answer";
  } & ChatAnswer
  | {
    type: "diff_query";
  } & DiffQuery
  | {
    type: "diff_answer";
  } & DiffAnswer
);

/**
 * A person's account on the server, 1–64 ASCII letters, digits, `-` or `_`.
 */
export type AccountId = string;

/**
 * Trades the one-time code sent to `email`, and the challenge its [`SignInStarted`] gave,
 * for an [`AccountSignedIn`].
 */
export type AccountSignInFinish = {
  challenge: ChallengeId;
  code: string;
  email: string;
};

/**
 * Asks the server to send a one-time code to `email`, if that person may sign in. The
 * answer, a [`SignInStarted`], is the same either way.
 */
export type AccountSignInStart = {
  email: string;
};

/**
 * The signed-in account. `token`, sent as a bearer token, is only in the answer to an
 * [`AccountSignInFinish`], for clients that keep no cookie.
 */
export type AccountSignedIn = {
  account: AccountId;
  email: string;
  token?: string;
};

/**
 * An agent's own id for a conversation, a lowercase UUID, which resumes it later.
 */
export type AgentSession = string;

/**
 * A program, such as `weacon-core`, and the exact build it is.
 */
export type App = {
  commit: string;
  name: string;
  version: string;
};

/**
 * Who made a change, such as one to a task.
 */
export type Author = (
  | {
    kind: "person";
    member: MemberId;
  }
  | {
    kind: "agent";
    owner: MemberId;
  }
);

/**
 * A Rust target a release is built for, such as `x86_64-unknown-linux-musl`.
 */
export type BuildTarget = string;

/**
 * A feature a peer supports, such as `chat.send`, named like a [`Code`]. A peer keeps the ones
 * it doesn't know, so a newer peer's list still parses.
 */
export type Capability = Code;

/**
 * One sign-in request, which its code and its tries belong to; 1–64 ASCII letters,
 * digits, `-` or `_`.
 */
export type ChallengeId = string;

export type ChatAction = (
  | {
    kind: "prompt";
    repo?: RepoId;
    text: string;
  }
  | {
    answer: PermissionAnswer;
    kind: "permission";
  }
  | {
    kind: "interrupt";
  }
);

export type ChatAnswer = {
  answer: ChatAnswerKind;
  chat: ChatId;
  workspace: WorkspaceId;
};

export type ChatAnswerKind = (
  | {
    kind: "events_lost";
  }
  | {
    events: Array<ChatEntry>;
    kind: "snapshot";
    runs: Array<RunStatus>;
    seq: number;
    waiting: Array<number>;
  }
  | {
    events: Array<ChatEntry>;
    kind: "window";
  }
);

/**
 * The core answers with receipts under `request`.
 */
export type ChatCommand = {
  action: ChatAction;
  chat: ChatId;
  request: RequestId;
  workspace: WorkspaceId;
};

/**
 * An event and its place in its chat's log: `seq` starts at 1 and grows by one.
 */
export type ChatEntry = {
  event: ChatEvent;
  seq: number;
};

export type ChatEvent = (
  | {
    kind: "prompt";
    repo?: RepoId;
    request: RequestId;
    text: string;
  }
  | {
    kind: "receipt";
    receipt: Receipt;
  }
  | {
    kind: "run";
    update: RunUpdate;
  }
  | {
    kind: "run_status";
    status: RunStatus;
  }
);

/**
 * A chat in a workspace, 1–64 ASCII letters, digits, `-` or `_`.
 */
export type ChatId = string;

/**
 * The answer is a [`ChatAnswer`], or for `subscribe` the [`ChatUpdate`]s themselves.
 */
export type ChatQuery = {
  chat: ChatId;
  query: ChatQueryKind;
  workspace: WorkspaceId;
};

export type ChatQueryKind = (
  | {
    after: number;
    kind: "subscribe";
  }
  | {
    kind: "snapshot";
  }
  | {
    before: number;
    kind: "window";
    limit: number;
  }
);

/**
 * Text deltas are never committed; they arrive only as run updates.
 */
export type ChatUpdate = {
  chat: ChatId;
  entry: ChatEntry;
  workspace: WorkspaceId;
};

/**
 * A log record from a client that has no log of its own, such as the web app: an event name and
 * typed ids. The receiver logs only events it knows, so no other text reaches its log.
 */
export type ClientLog = {
  code?: Code;
  level: Level;
  msg: LogEvent;
  request_id?: RequestId;
  run_id?: RunId;
};

/**
 * A dotted code such as `store.full`: two or more lowercase words joined by dots. It names
 * errors, and capabilities too.
 */
export type Code = string;

/**
 * A full git commit id: 40 lowercase hex digits.
 */
export type Commit = string;

/**
 * A git commit, as 40 or 64 lowercase hex digits.
 */
export type CommitId = string;

/**
 * From the core to a client that has `diff.view`, in answer to a [`DiffQuery`].
 */
export type DiffAnswer = {
  answer: DiffAnswerKind;
  chat: ChatId;
  request: RequestId;
  workspace: WorkspaceId;
};

export type DiffAnswerKind = (
  | {
    file: DiffFile;
    files: number;
    kind: "file";
  }
  | {
    kind: "unchanged";
  }
);

export type DiffContent = (
  | {
    hunks: Array<Hunk>;
    kind: "text";
  }
  | {
    kind: "binary";
  }
  | {
    kind: "too_large";
  }
);

export type DiffFile = {
  change: FileChange;
  content: DiffContent;
  /**
   * Where the file is now, or was when it was removed, relative to the worktree.
   */
  path: string;
};

/**
 * One line without its `\n`, so a `\r` before it stays in `text`.
 */
export type DiffLine = {
  /**
   * The file's last line, when it has no `\n`.
   */
  no_newline?: boolean;
  op: LineOp;
  text: string;
};

/**
 * From a client to a core that offers `diff.view`. Each answer is a [`DiffAnswer`] that echoes
 * `request`.
 */
export type DiffQuery = {
  chat: ChatId;
  query: DiffQueryKind;
  request: RequestId;
  workspace: WorkspaceId;
};

export type DiffQueryKind = (
  | {
    kind: "changes";
  }
);

/**
 * A pin for each anchor a doc links to, all computed at `commit`.
 */
export type DocAnchors = {
  commit: CommitId;
  pins: Array<DocPin>;
};

/**
 * What one revision of a doc says: its title, its Markdown, and the anchors it links to,
 * unless it links to none.
 */
export type DocContent = {
  anchors?: DocAnchors;
  body: string;
  title: string;
};

/**
 * A doc's revisions, newest first, from the server.
 */
export type DocHistory = {
  revisions: Array<DocRevision>;
};

/**
 * A doc in a workspace, 1–64 ASCII letters, digits, `-` or `_`.
 */
export type DocId = string;

/**
 * A workspace's docs, from the server.
 */
export type DocList = {
  docs: Array<DocSummary>;
};

/**
 * An anchor a doc links to, with the fingerprint the code had at the revision's commit:
 * `sym:weacon_protocol::frame::encode_line#0123456789abcdef`, in printable ASCII. The core
 * computes both; the server only checks their shape.
 */
export type DocPin = string;

/**
 * A doc at one revision; `version` rises with each revision. The server sends it to clients
 * that have the capability `workspace.docs`.
 */
export type DocRecord = {
  content: DocContent;
  id: DocId;
  version: number;
};

/**
 * Asks the server for a new revision of a doc that the client read at `version`; the server
 * refuses when the doc has changed since. The answer is its [`DocRecord`].
 */
export type DocRevise = {
  content: DocContent;
  version: number;
};

/**
 * Who made one revision of a doc, who accepted it, and when, in seconds since the Unix
 * epoch.
 */
export type DocRevision = {
  accepted_by: MemberId;
  at: number;
  author: Author;
  version: number;
};

/**
 * A doc in a [`DocList`], without its body.
 */
export type DocSummary = {
  id: DocId;
  title: string;
  version: number;
};

/**
 * An `https://` address of printable ASCII, at most 2,048 bytes, with a plain host name and
 * optionally a port: no user name or password.
 */
export type DownloadUrl = string;

export type ErrorBody = {
  code: Code;
  /**
   * Whether the same request can succeed if sent again later.
   */
  retryable: boolean;
};

export type FileChange = (
  | {
    kind: "added";
  }
  | {
    kind: "removed";
  }
  | {
    kind: "modified";
  }
  | {
    from: string;
    kind: "renamed";
  }
);

/**
 * The first message on every connection, from the side that connects.
 */
export type Hello = {
  app: App;
  capabilities?: Array<Capability>;
  protocol: number;
};

/**
 * A run of changed lines and their context. Lines count from 1; the start of a side with no
 * lines, as the old side of an added file, is 0.
 */
export type Hunk = {
  lines: Array<DiffLine>;
  new_start: number;
  old_start: number;
};

export type Level = (
  | "trace"
  | "debug"
  | "info"
  | "warn"
  | "error"
  | "fatal"
);

export type LineOp = (
  | (
    | "context"
    | "added"
    | "removed"
  )
);

/**
 * What happened, such as `run started`: one to four words of lowercase letters or `_`, one
 * space apart, at most 40 bytes; no digits, so ids and secrets can't be spelt in it.
 */
export type LogEvent = string;

/**
 * A member of a workspace, 1–64 ASCII letters, digits, `-` or `_`.
 */
export type MemberId = string;

/**
 * Where an accepted command stands. It ends `completed`, `interrupted`, `failed` or `unknown`.
 * `unknown` means it may or may not have run, as when a restart cut it off; the client must not
 * send it again under a new id.
 */
export type Outcome = (
  | {
    state: "accepted";
  }
  | {
    state: "completed";
  }
  | {
    state: "interrupted";
  }
  | {
    code: Code;
    state: "failed";
  }
  | {
    state: "unknown";
  }
);

/**
 * A person's answer to a [`RunEvent::PermissionAsked`].
 */
export type PermissionAnswer = {
  allow: boolean;
  prompt: number;
  run: RunId;
};

/**
 * An element someone picked on a local web page, sent to the core for a chat. The sender cuts
 * the HTML short and shrinks the screenshot so the message fits in one frame.
 */
export type Pick = {
  /**
   * The element's outer HTML.
   */
  html: string;
  request: RequestId;
  /**
   * What the element looked like: a PNG, base64-encoded. None when the page couldn't be
   * captured.
   */
  screenshot_png?: (
    | string
    | null
  );
  /**
   * A CSS selector that finds the element on the page.
   */
  selector: string;
  title: string;
  url: string;
};

/**
 * The daemon's word on a command, sent when it is accepted and again when it ends.
 */
export type Receipt = {
  request: RequestId;
} & (
  | {
    state: "accepted";
  }
  | {
    state: "completed";
  }
  | {
    state: "interrupted";
  }
  | {
    code: Code;
    state: "failed";
  }
  | {
    state: "unknown";
  }
);

/**
 * One target's binary in a release.
 */
export type ReleaseBuild = {
  sha256: Sha256;
  size: number;
  target: BuildTarget;
  url: DownloadUrl;
};

/**
 * A release, as its signed manifest describes it; a core reads it to update itself.
 */
export type ReleaseManifest = {
  /**
   * At most one for each target.
   */
  builds: Array<ReleaseBuild>;
  commit: Commit;
  protocol: number;
  /**
   * When the release was signed, in seconds since the Unix epoch, so a core can tell a
   * manifest that is being held back from the newest one.
   */
  published_at: number;
  version: ReleaseVersion;
};

/**
 * A release's `major.minor.patch`, ordered by its numbers.
 */
export type ReleaseVersion = string;

/**
 * A git repository the owner registered for a workspace on their machine, 1–64 ASCII
 * letters, digits, `-` or `_`.
 */
export type RepoId = string;

/**
 * A command's id, chosen by the client and kept across reconnects and retries. Sending the
 * same id again returns the first receipt instead of repeating the work; the same id with
 * different content is refused with the error `request.conflict`.
 */
export type RequestId = string;

/**
 * What an agent's run reports, in order. Each turn ends with `finished`.
 */
export type RunEvent = (
  | {
    event: "started";
    session: AgentSession;
  }
  | {
    event: "text_delta";
    text: string;
  }
  | {
    event: "text";
    text: string;
  }
  | {
    call: string;
    event: "tool_call";
    input: unknown;
    tool: string;
  }
  | {
    call: string;
    event: "tool_done";
    failed: boolean;
  }
  | {
    event: "permission_asked";
    input: unknown;
    prompt: number;
    run: RunId;
    tool: string;
  }
  | {
    event: "permission_withdrawn";
    prompt: number;
    run: RunId;
  }
  | {
    event: "finished";
    outcome: Outcome;
  }
);

/**
 * The core's id for one run of an agent process, 1–64 ASCII letters, digits, `-` or `_`.
 * Prompts carry it, so an answer only ever reaches the run that asked.
 */
export type RunId = string;

export type RunState = (
  | "starting"
  | "running"
  | "waiting"
  | "idle"
  | "sleeping"
  | "ended"
);

/**
 * Where a run stands, as its chat's log records it.
 */
export type RunStatus = {
  run: RunId;
  state: RunState;
};

/**
 * One event of the run `run`, in the order the run reported it.
 */
export type RunUpdate = {
  event: RunEvent;
  run: RunId;
};

/**
 * A SHA-256 digest: 64 lowercase hex digits.
 */
export type Sha256 = string;

/**
 * Trades the one-time code sent to `email` for a [`SignedIn`]. `challenge` is the one its
 * [`SignInStarted`] gave; a server that hands out challenges refuses a finish without one.
 */
export type SignInFinish = {
  challenge?: ChallengeId;
  code: string;
  email: string;
  workspace: WorkspaceId;
};

/**
 * Sign-in to one workspace, which [`AccountSignInStart`] replaces: asks the server to send
 * a one-time code to `email`, if it is a member of `workspace`. The answer, a [`SignInStarted`], is the same either way.
 */
export type SignInStart = {
  email: string;
  workspace: WorkspaceId;
};

/**
 * The answer to a [`SignInStart`], for members and strangers alike: the challenge to finish
 * it with.
 */
export type SignInStarted = {
  challenge: ChallengeId;
};

/**
 * A signed-in member and the session token that proves it, sent as a bearer token.
 */
export type SignedIn = {
  member: MemberId;
  token: string;
};

/**
 * Asks the server to change a task that the client read at `version`; the server refuses
 * when the task has changed since. The answer is its [`TaskRecord`].
 */
export type TaskChange = {
  edit: TaskEditBody;
  version: number;
};

/**
 * Asks the server for a new task, placed in front of `before`, or last without one. The
 * answer is its [`TaskRecord`].
 */
export type TaskCreate = {
  before?: TaskId;
  title: string;
};

/**
 * One edit to a task, inside a [`TaskChange`].
 */
export type TaskEditBody = (
  | {
    kind: "title";
    title: string;
  }
  | {
    kind: "status";
    status: TaskStatus;
  }
  | {
    before?: TaskId;
    kind: "move";
  }
);

/**
 * A task in a workspace, 1–64 ASCII letters, digits, `-` or `_`.
 */
export type TaskId = string;

/**
 * A workspace's tasks in their order, from the server.
 */
export type TaskList = {
  tasks: Array<TaskRecord>;
};

/**
 * A task as it travels: `order` is its order key and `version` rises with each change. The
 * server sends it to clients that have the capability `workspace.tasks`.
 */
export type TaskRecord = {
  id: TaskId;
  order: string;
  status: TaskStatus;
  title: string;
  version: number;
};

/**
 * How far along a task is.
 */
export type TaskStatus = (
  | (
    | "todo"
    | "in_progress"
    | "done"
  )
);

/**
 * The answer to a [`Hello`] that speaks the same protocol.
 */
export type Welcome = {
  app: App;
  capabilities?: Array<Capability>;
};

/**
 * What changed in a workspace.
 */
export type WorkspaceChange = (
  | {
    kind: "member_joined";
    member: MemberId;
  }
  | {
    kind: "task_changed";
    task: TaskId;
    version: number;
  }
  | {
    doc: DocId;
    kind: "doc_changed";
    version: number;
  }
);

/**
 * Asks the server for a new workspace, which the asker owns. The answer is its
 * [`WorkspaceSummary`].
 */
export type WorkspaceCreate = {
  name: string;
};

/**
 * A workspace on the server, 1–64 ASCII letters, digits, `-` or `_`.
 */
export type WorkspaceId = string;

/**
 * The workspaces the signed-in account is a member of.
 */
export type WorkspaceList = {
  workspaces: Array<WorkspaceSummary>;
};

/**
 * What a member may do in a workspace.
 */
export type WorkspaceRole = (
  | "owner"
  | "member"
);

/**
 * A workspace as one of its members sees it, with their `role` in it.
 */
export type WorkspaceSummary = {
  id: WorkspaceId;
  name: string;
  role: WorkspaceRole;
};

/**
 * One change to `workspace`. `seq` grows strictly within a workspace, and a feed opened with
 * `since=N` sends every change after `N`.
 */
export type WorkspaceUpdate = {
  change: WorkspaceChange;
  seq: number;
  workspace: WorkspaceId;
};
