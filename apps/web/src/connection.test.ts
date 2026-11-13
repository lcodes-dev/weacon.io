import assert from "node:assert/strict";
import { test } from "node:test";

import { Connection, type Listener, type Socket, type Status, parse, statusText } from "./connection.ts";
import type { App } from "./protocol.ts";

const ME: App = { name: "weacon-web", version: "0.1.0", commit: "1a2b3c4" };
const SERVER: App = { name: "weacon-server", version: "0.2.0", commit: "5e6f7a8" };

class FakeSocket implements Socket {
  sent: unknown[] = [];
  closed = false;
  #listeners = new Map<string, ((event: { data: unknown }) => void)[]>();

  send(text: string): void {
    this.sent.push(JSON.parse(text));
  }

  close(): void {
    this.closed = true;
  }

  addEventListener(type: string, listener: (event: { data: unknown }) => void): void {
    this.#listeners.set(type, [...(this.#listeners.get(type) ?? []), listener]);
  }

  fire(type: string, data?: unknown): void {
    this.#listeners.get(type)?.forEach((listener) => listener({ data }));
  }
}

function open(): { socket: FakeSocket; connection: Connection; statuses: Status[] } {
  const socket = new FakeSocket();
  const statuses: Status[] = [];
  const listener: Listener = { status: (status) => statuses.push(status) };
  const connection = new Connection(socket, ME, listener);
  socket.fire("open");
  return { socket, connection, statuses };
}

const welcome = (capabilities: string[]) => JSON.stringify({ id: 1, type: "welcome", app: SERVER, capabilities });

test("it says hello, and sends only once welcomed", () => {
  const { socket, connection, statuses } = open();
  connection.send({ id: 0, type: "error", code: "web.early", retryable: false });
  socket.fire("message", welcome(["run.stream", "log.forward"]));
  socket.fire("message", welcome(["run.stream"]));
  connection.send({ id: 0, type: "error", code: "web.crashed", retryable: false });
  socket.fire("close");

  assert.deepEqual(socket.sent, [
    { id: 1, type: "hello", protocol: 1, app: ME, capabilities: ["run.stream"] },
    { id: 0, type: "error", code: "web.crashed", retryable: false },
  ]);
  assert.deepEqual(statuses, [
    { state: "live", server: SERVER, capabilities: ["run.stream", "log.forward"] },
    { state: "ended", code: "connection.lost" },
  ]);
});

test("only a server that streams runs is welcome, and refusals say why", () => {
  const ends = [
    welcome([]),
    JSON.stringify({ id: 1, type: "welcome", capabilities: ["run.stream"] }),
    JSON.stringify({ id: 1, type: "welcome", app: { ...SERVER, commit: 7 }, capabilities: ["run.stream"] }),
    JSON.stringify({ id: 1, type: "welcome", app: SERVER, capabilities: "run.stream.v2" }),
    JSON.stringify({ id: 1, type: "error", code: "protocol.unsupported", retryable: false }),
    JSON.stringify({ id: 1, type: "error", code: "store.full", retryable: false }),
    "garbled",
  ].map((frame) => {
    const { socket, connection, statuses } = open();
    socket.fire("message", frame);
    socket.fire("close");
    connection.send({ id: 0, type: "error", code: "web.crashed", retryable: false });
    return { statuses, closed: socket.closed, sent: socket.sent.length };
  });

  assert.deepEqual(
    ends.map(({ statuses }) => statuses),
    [
      [{ state: "ended", code: "connection.server_too_old" }],
      [{ state: "ended", code: "connection.refused" }],
      [{ state: "ended", code: "connection.refused" }],
      [{ state: "ended", code: "connection.refused" }],
      [{ state: "ended", code: "connection.other_protocol" }],
      [{ state: "ended", code: "connection.refused" }],
      [{ state: "ended", code: "connection.refused" }],
    ],
  );
  assert.ok(ends.every(({ closed, sent }) => closed && sent === 1));
});

test("a socket that closes before any answer is refused", () => {
  const { socket, statuses } = open();
  socket.fire("close");

  assert.deepEqual(statuses, [{ state: "ended", code: "connection.refused" }]);
});

test("frames that aren't messages are dropped", () => {
  assert.equal(parse(new ArrayBuffer(1)), undefined);
  assert.equal(parse("[1]"), undefined);
  assert.equal(parse('{"type":3}'), undefined);
  assert.equal(parse("null"), undefined);
  assert.deepEqual(parse('{"id":0,"type":"newer"}'), { id: 0, type: "newer" });
});

test("a server that doesn't welcome the app in time ends the connection", (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const late = open();
  const prompt = open();
  context.mock.timers.tick(9_999);
  prompt.socket.fire("message", welcome(["run.stream"]));
  context.mock.timers.tick(1);

  assert.deepEqual(late.statuses, [{ state: "ended", code: "connection.no_answer" }]);
  assert.ok(late.socket.closed);
  assert.equal(prompt.statuses.at(-1)?.state, "live");
});

test("the header says how the connection stands", () => {
  assert.equal(statusText({ state: "connecting" }), "Connecting to the server");
  assert.equal(statusText({ state: "live", server: SERVER, capabilities: [] }), "Connected");
  assert.equal(statusText({ state: "ended", code: "connection.lost" }), "The server closed the connection");
  assert.equal(statusText({ state: "ended", code: "connection.refused" }), "The server can't be reached");
  assert.equal(statusText({ state: "ended", code: "connection.no_answer" }), "The server didn't answer");
  assert.equal(
    statusText({ state: "ended", code: "connection.other_protocol" }),
    "The server runs another version of Weacon; reload the page",
  );
  assert.equal(statusText({ state: "ended", code: "connection.server_too_old" }), "The server is too old for this app");
});
