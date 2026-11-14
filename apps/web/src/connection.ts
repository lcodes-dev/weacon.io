import { PROTOCOL } from "../../../tooling/ts/version.ts";
import { type Incoming, parse } from "./incoming.ts";
import type { App, Envelope } from "./protocol.ts";

/** Every capability the web app speaks of, in its hello or in the server's welcome. */
export type WebCapability = "run.stream" | "workspace.feed" | "workspace.chats" | "diff.view" | "chat.send" | "chat.events" | "log.forward";

/** The hello's envelope id; a connection's requests take the ids after it. */
export const HELLO_ID = 1;

/** What a connection asks of the server: what it must offer, and what else this app can use. */
export type Asks = { needs: WebCapability; offers: WebCapability[] };

/** Whether the server is live and offers `capability`. */
export function offers(status: Status, capability: WebCapability): boolean {
  return status.state === "live" && status.capabilities.includes(capability);
}

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

/** Runs `run` once `ms` have passed; injected, so tests can run it at will. */
export type Later = (run: () => void, ms: number) => void;

/** Told each change of status, and each message after the welcome. */
export type Listener = {
  status(status: Status): void;
  message(envelope: Incoming): void;
};

/** Says hello as `me`, with what it `asks`, once `socket` opens, then passes on what the server sends. */
export class Connection {
  #socket: Socket;
  #listener: Listener;
  #needs: WebCapability;
  #status: Status = { state: "connecting" };

  constructor(socket: Socket, me: App, listener: Listener, asks: Asks, later: Later) {
    this.#socket = socket;
    this.#listener = listener;
    this.#needs = asks.needs;
    later(() => {
      if (this.#status.state === "connecting") {
        this.#end("connection.no_answer");
      }
    }, WELCOME_MS);
    socket.addEventListener("open", () => {
      this.#send({ id: HELLO_ID, type: "hello", protocol: PROTOCOL, app: me, capabilities: [asks.needs, ...asks.offers] });
    });
    socket.addEventListener("message", ({ data }) => {
      const envelope = parse(data);
      if (this.#status.state === "connecting") {
        this.#welcome(envelope);
      } else if (this.#status.state === "live" && envelope !== undefined) {
        this.#listener.message(envelope);
      }
    });
    socket.addEventListener("close", () => {
      if (this.#status.state !== "ended") {
        this.#end(this.#status.state === "live" ? "connection.lost" : "connection.refused");
      }
    });
  }

  /** Sends `envelope` once the server has welcomed this app; drops it otherwise. */
  send(envelope: Envelope): void {
    if (this.#status.state === "live") {
      this.#send(envelope);
    }
  }

  #welcome(envelope: Incoming | undefined): void {
    if (envelope?.type !== "welcome") {
      const other = envelope?.type === "error" && envelope.code === "protocol.unsupported";
      this.#end(other ? "connection.other_protocol" : "connection.refused");
      return;
    }
    const capabilities = envelope.capabilities ?? [];
    if (!capabilities.includes(this.#needs)) {
      this.#end("connection.server_too_old");
      return;
    }
    this.#status = { state: "live", server: envelope.app, capabilities };
    this.#listener.status(this.#status);
  }

  #end(code: End): void {
    this.#status = { state: "ended", code };
    this.#socket.close();
    this.#listener.status(this.#status);
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
