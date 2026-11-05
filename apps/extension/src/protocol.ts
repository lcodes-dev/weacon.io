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
);

/**
 * A program, such as `weacon-desktop`, and the exact build it is.
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
 * The answer to a [`Hello`] that speaks the same protocol.
 */
export type Welcome = {
  app: App;
  capabilities?: Array<Capability>;
};
