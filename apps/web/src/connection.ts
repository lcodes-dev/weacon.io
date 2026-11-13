import { PROTOCOL } from "../../../tooling/ts/version.ts";
import { DIFF_VIEW } from "./diff/changes.ts";
import type { App, Envelope } from "./protocol.ts";

// What the web app needs the server to offer.
const NEEDS = "run.stream";
// What else it can do with a server that offers the same.
const OFFERS = [DIFF_VIEW];

/** Why a connection ended. */
export type End =
  | "connection.lost"
  | "connection.refused"
  | "connection.no_answer"
  | "connection.other_protocol"
  | "connection.server_too_old";

// How long the server has to welcome this app once the socket is made.
const WELCOME_MS = 10_000;

/** The part of a browser `WebSocket` this file uses. */
export type Socket = {
  send(text: string): void;
  close(): void;
  addEventListener(type: "open" | "close", listener: () => void): void;
  addEventListener(type: "message", listener: (event: { data: unknown }) => void): void;
};

/** What happened to the connection, for the view. */
export type Status =
  | { state: "connecting" }
  | { state: "live"; server: App; capabilities: string[] }
  | { state: "ended"; code: End };

/** Told each change of status, and each message after the welcome. */
export type Listener = {
  status(status: Status): void;
  message(envelope: Envelope): void;
};

/** Says hello as `me` once `socket` opens, then passes on what the server sends. */
export class Connection {
  #socket: Socket;
  #status: Status = { state: "connecting" };
  #deadline: ReturnType<typeof setTimeout>;

  constructor(socket: Socket, me: App, listener: Listener) {
    this.#socket = socket;
    this.#deadline = setTimeout(() => {
      if (this.#status.state === "connecting") {
        this.#end(listener, "connection.no_answer");
      }
    }, WELCOME_MS);
    socket.addEventListener("open", () => {
      this.#send({ id: 1, type: "hello", protocol: PROTOCOL, app: me, capabilities: [NEEDS, ...OFFERS] });
    });
    socket.addEventListener("message", ({ data }) => {
      const envelope = parse(data);
      if (this.#status.state === "connecting") {
        this.#welcome(envelope, listener);
      } else if (this.#status.state === "live" && envelope !== undefined) {
        listener.message(envelope);
      }
    });
    socket.addEventListener("close", () => {
      if (this.#status.state !== "ended") {
        this.#end(listener, this.#status.state === "live" ? "connection.lost" : "connection.refused");
      }
    });
  }

  /** Sends `envelope` once the server has welcomed this app; drops it otherwise. */
  send(envelope: Envelope): void {
    if (this.#status.state === "live") {
      this.#send(envelope);
    }
  }

  #welcome(envelope: Envelope | undefined, listener: Listener): void {
    if (envelope?.type !== "welcome") {
      const other = envelope?.type === "error" && envelope.code === "protocol.unsupported";
      this.#end(listener, other ? "connection.other_protocol" : "connection.refused");
      return;
    }
    const capabilities: unknown = envelope.capabilities ?? [];
    if (!isApp(envelope.app) || !isStrings(capabilities)) {
      this.#end(listener, "connection.refused");
      return;
    }
    if (!capabilities.includes(NEEDS)) {
      this.#end(listener, "connection.server_too_old");
      return;
    }
    clearTimeout(this.#deadline);
    this.#status = { state: "live", server: envelope.app, capabilities };
    listener.status(this.#status);
  }

  #end(listener: Listener, code: End): void {
    clearTimeout(this.#deadline);
    this.#status = { state: "ended", code };
    this.#socket.close();
    listener.status(this.#status);
  }

  #send(envelope: Envelope): void {
    this.#socket.send(JSON.stringify(envelope));
  }
}

const ENDINGS: Record<End, string> = {
  "connection.lost": "The server closed the connection",
  "connection.refused": "The server can't be reached",
  "connection.no_answer": "The server didn't answer",
  "connection.other_protocol": "The server runs another version of Weacon; reload the page",
  "connection.server_too_old": "The server is too old for this app",
};

/** What the header says about the connection. */
export function statusText(status: Status): string {
  switch (status.state) {
    case "connecting":
      return "Connecting to the server";
    case "live":
      return "Connected";
    case "ended":
      return ENDINGS[status.code];
  }
}

// The welcome's fields, checked before the connection goes live; `Feed` checks run messages.
function isApp(app: unknown): app is App {
  return (
    typeof app === "object" &&
    app !== null &&
    ["name", "version", "commit"].every((key) => typeof (app as Record<string, unknown>)[key] === "string")
  );
}

function isStrings(list: unknown): list is string[] {
  return Array.isArray(list) && list.every((item) => typeof item === "string");
}

/** A message from the server, or `undefined` when a frame isn't one. */
export function parse(data: unknown): Envelope | undefined {
  if (typeof data !== "string") {
    return undefined;
  }
  try {
    const value: unknown = JSON.parse(data);
    const fit = typeof value === "object" && value !== null && "type" in value && typeof value.type === "string";
    return fit ? (value as Envelope) : undefined;
  } catch {
    return undefined;
  }
}
