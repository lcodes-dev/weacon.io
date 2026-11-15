import assert from "node:assert/strict";
import { test } from "node:test";

import { PROTOCOL } from "../../../tooling/ts/version.ts";
import { Failure, NEEDS, type Port, connect, report } from "./core.ts";
import type { Incoming } from "./incoming.ts";
import type { Envelope } from "./protocol.ts";

const ME = { name: "weacon-extension", version: "0.1.0", commit: "1a2b3c4" };
const CORE = { name: "weacon-core", version: "0.1.0", commit: "1a2b3c4" };
const HANG_UP = Symbol("hang up");
const LIST: Envelope = { id: 2, type: "chats_query", query: { kind: "list" } };

// A port whose core answers each message with `answer`'s replies, or hangs up on undefined.
function port(answer: (sent: Envelope) => Array<Envelope | string | typeof HANG_UP> | undefined) {
  const sent: Envelope[] = [];
  let receive = (_: unknown) => {};
  let hangUp = () => {};
  let disconnected = false;
  const fake: Port = {
    postMessage(message) {
      sent.push(message);
      queueMicrotask(() => {
        const replies = answer(message);
        if (replies === undefined) {
          hangUp();
        } else {
          replies.forEach((reply) => (reply === HANG_UP ? hangUp() : receive(reply)));
        }
      });
    },
    disconnect() {
      disconnected = true;
    },
    onMessage: { addListener: (listener) => (receive = listener) },
    onDisconnect: { addListener: (listener) => (hangUp = listener) },
  };
  return { fake, sent, disconnected: () => disconnected, push: (message: Envelope) => receive(message), hangUp: () => hangUp() };
}

function welcome(capabilities: string[]): Envelope {
  return { id: 1, type: "welcome", app: CORE, capabilities };
}

function core(replies: (sent: Envelope) => Envelope[] | undefined, capabilities = NEEDS) {
  return port((sent) => (sent.type === "hello" ? [welcome(capabilities)] : replies(sent)));
}

async function failure(promise: Promise<unknown>): Promise<string> {
  const error = await promise.then(
    () => undefined,
    (error: unknown) => error,
  );
  assert.ok(error instanceof Failure, String(error));
  return error.code;
}

test("a conversation greets the core first and gets each reply by its id", async () => {
  const listed: Envelope = { id: 2, type: "chats_answer", answer: { kind: "list", chats: [] } };
  const { fake, sent } = core(() => [{ id: 9, type: "receipt", request: "x", state: "completed" }, listed]);
  const heard: Incoming[] = [];

  const link = await connect(fake, ME, (message) => heard.push(message), () => {});
  const reply = await link.ask({ ...LIST, id: link.next() });

  assert.deepEqual(sent, [{ id: 1, type: "hello", protocol: PROTOCOL, app: ME }, LIST]);
  assert.deepEqual(reply, listed);
  assert.deepEqual(heard, [{ id: 9, type: "receipt", request: "x", state: "completed" }]);
});

test("what no one waits for, pushes and malformed messages aside, is heard", async () => {
  const pushed: Envelope = { id: 0, type: "chat_answer", workspace: "w-1", chat: "c-1", answer: { kind: "events_lost" } };
  const refused: Envelope = { id: 3, type: "error", code: "chat.held", retryable: false };
  const { fake, push } = core(() => []);
  const heard: Incoming[] = [];
  await connect(fake, ME, (message) => heard.push(message), () => {});

  push(pushed);
  push(refused);
  push({ id: 4, type: "chat", workspace: "w-1", chat: "c-1", entry: { seq: 0, event: { kind: "outside_prompt", text: "x" } } });

  assert.deepEqual(heard, [pushed, refused]);
});

test("an accepted receipt isn't the end, and a core that goes quiet fails the wait", async () => {
  const pick: Envelope = { id: 2, type: "pick", request: "p-1", url: "http://localhost/", title: "", selector: "p", html: "<p></p>" };
  const accepted: Envelope = { id: 2, type: "receipt", request: "p-1", state: "accepted" };
  const completed: Envelope = { id: 2, type: "receipt", request: "p-1", state: "completed" };
  const twice = await connect(core(() => [accepted, completed]).fake, ME, () => {}, () => {});
  const quiet = await connect(core(() => [accepted]).fake, ME, () => {}, () => {});

  assert.deepEqual(await twice.ask(pick), completed);
  assert.equal(await failure(quiet.ask(pick, 10)), "extension.core_no_reply");
});

test("a core without what the panel needs is told apart from one that isn't there", async () => {
  const other = port(() => [{ id: 1, type: "error", code: "protocol.unsupported", retryable: false }]);
  const refused = port(() => [{ id: 1, type: "error", code: "handshake.required", retryable: false }]);
  const gone = port(() => undefined);
  const old = core(() => [], NEEDS.slice(1));

  assert.equal(await failure(connect(other.fake, ME, () => {}, () => {})), "extension.core_other_protocol");
  assert.equal(await failure(connect(refused.fake, ME, () => {}, () => {})), "extension.core_refused");
  assert.equal(await failure(connect(gone.fake, ME, () => {}, () => {})), "extension.core_unreachable");
  assert.equal(await failure(connect(old.fake, ME, () => {}, () => {})), "extension.core_too_old");
  assert.equal(await failure(connect(port(() => []).fake, ME, () => {}, () => {}, 10)), "extension.core_no_reply");
  assert.ok(old.disconnected() && gone.disconnected());
});

test("a closed port fails what waits and what is sent after, and says it closed", async () => {
  const { fake, hangUp } = core(() => []);
  let closed = 0;
  const link = await connect(fake, ME, () => {}, () => (closed += 1));
  const waiting = link.ask(LIST);

  hangUp();

  assert.equal(await failure(waiting), "extension.core_unreachable");
  assert.equal(await failure(link.ask(LIST)), "extension.core_unreachable");
  assert.throws(() => link.send(LIST), Failure);
  assert.equal(closed, 1);
});

test("closing the link disconnects and fails what waits", async () => {
  const { fake, disconnected } = core(() => []);
  const link = await connect(fake, ME, () => {}, () => {});
  const waiting = link.ask(LIST);

  link.close();

  assert.ok(disconnected());
  assert.equal(await failure(waiting), "extension.core_unreachable");
  assert.equal([link.next(), link.next()].join(), "2,3");
});

test("failures are reported by code after a hello, and quietly dropped without a core", async () => {
  const told = core(() => [], []);
  const gone = port(() => undefined);

  await report(told.fake, ME, "extension.capture_failed");
  await report(gone.fake, ME, "extension.capture_failed");

  assert.deepEqual(told.sent.at(-1), { id: 2, type: "error", code: "extension.capture_failed", retryable: false });
  assert.ok(told.disconnected() && gone.disconnected());
  assert.equal(gone.sent.length, 1);
});
