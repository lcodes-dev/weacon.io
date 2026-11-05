// Generated from crates/protocol/schema.json by tools/generate-types.ts. Don't edit it.

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
);

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
 * A feature a peer supports, such as `chat.send`, named like a [`Code`]. A peer keeps the ones
 * it doesn't know, so a newer peer's list still parses.
 */
export type Capability = Code;

/**
 * A dotted code such as `store.full`: two or more lowercase words joined by dots. It names
 * errors, and capabilities too.
 */
export type Code = string;

export type ErrorBody = {
  code: Code;
  /**
   * Whether the same request can succeed if sent again later.
   */
  retryable: boolean;
};

/**
 * The first message on every connection, from the side that connects.
 */
export type Hello = {
  app: App;
  capabilities?: Array<Capability>;
  protocol: number;
};

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

/**
 * One event of the run `run`, in the order the run reported it.
 */
export type RunUpdate = {
  event: RunEvent;
  run: RunId;
};

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
);

/**
 * A workspace on the server, 1–64 ASCII letters, digits, `-` or `_`.
 */
export type WorkspaceId = string;

/**
 * One change to `workspace`. `seq` grows strictly within a workspace, and a feed opened with
 * `since=N` sends every change after `N`.
 */
export type WorkspaceUpdate = {
  change: WorkspaceChange;
  seq: number;
  workspace: WorkspaceId;
};
