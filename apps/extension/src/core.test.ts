import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import { Failure, PROTOCOL, type Port, app, deliver, report } from "./core.ts";
import type { Envelope, Pick } from "./protocol.ts";

const ME = { name: "weacon-extension", version: "0.1.0", commit: "1a2b3c4" };
const CORE = { name: "weacon-core", version: "0.1.0", commit: "1a2b3c4" };
const PICK: Pick = {
  request: "p-1",
  url: "http://localhost:3000/",
  title: "Home",
  selector: "#app > button",
  html: "<button>Save</button>",
  screenshot_png: "iVBORw0KGgo=",
};

const HANG_UP = Symbol("hang up");

// A port whose core answers each message with `answer`'s replies, or hangs up on undefined.
function port(answer: (sent: Envelope) => unknown[] | undefined) {
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
  return { fake, sent, disconnected: () => disconnected };
}

function welcome(capabilities: string[]) {
  return { id: 1, type: "welcome", app: CORE, capabilities };
}

function core(pickReplies: unknown[] | undefined, capabilities = ["element.pick"]) {
  return port((sent) => (sent.type === "hello" ? [welcome(capabilities)] : pickReplies));
}

async function failure(promise: Promise<void>): Promise<string> {
  const error = await promise.then(
    () => undefined,
    (error: unknown) => error,
  );
  assert.ok(error instanceof Failure, String(error));
  return error.code;
}

test("a pick follows the hello and ends with its receipt", async () => {
  const { fake, sent, disconnected } = core([
    { id: 7, type: "receipt", request: "other", state: "completed" },
    "not an envelope",
    { id: 2, type: "receipt", request: "p-1", state: "completed" },
  ]);

  await deliver(fake, ME, PICK);

  assert.deepEqual(sent, [
    { id: 1, type: "hello", protocol: PROTOCOL, app: ME },
    { id: 2, type: "pick", ...PICK },
  ]);
  assert.ok(disconnected());
});

test("a refused or failed pick fails with the core's code", async () => {
  const failed = { id: 2, type: "receipt", request: "p-1", state: "failed", code: "pick.not_kept" };
  const conflict = { id: 2, type: "error", code: "request.conflict", retryable: false };
  const odd = { id: 2, type: "welcome", app: CORE };

  assert.equal(await failure(deliver(core([failed]).fake, ME, PICK)), "pick.not_kept");
  assert.equal(await failure(deliver(core([conflict]).fake, ME, PICK)), "request.conflict");
  assert.equal(await failure(deliver(core([odd]).fake, ME, PICK)), "extension.unexpected_reply");
});

test("a core that can't take picks is told apart from one that isn't there", async () => {
  const other = port(() => [{ id: 1, type: "error", code: "protocol.unsupported", retryable: false }]);
  const refused = port(() => [{ id: 1, type: "error", code: "handshake.required", retryable: false }]);
  const gone = port(() => undefined);
  const old = core([], []);

  assert.equal(await failure(deliver(other.fake, ME, PICK)), "extension.core_other_protocol");
  assert.equal(await failure(deliver(refused.fake, ME, PICK)), "extension.core_refused");
  assert.equal(await failure(deliver(gone.fake, ME, PICK)), "extension.core_unreachable");
  assert.equal(await failure(deliver(old.fake, ME, PICK)), "extension.core_too_old");
  assert.equal(old.sent.length, 1);
  assert.ok(gone.disconnected());
});

test("an accepted receipt isn't the end, and a core that goes quiet fails the wait", async () => {
  const accepted = { id: 2, type: "receipt", request: "p-1", state: "accepted" };
  const completed = { id: 2, type: "receipt", request: "p-1", state: "completed" };
  const quiet = port((sent) => (sent.type === "hello" ? [welcome(["element.pick"])] : []));

  await deliver(core([accepted, completed]).fake, ME, PICK);
  assert.equal(await failure(deliver(core([accepted]).fake, ME, PICK, 10)), "extension.core_no_reply");
  assert.equal(await failure(deliver(quiet.fake, ME, PICK, 10)), "extension.core_no_reply");
  assert.ok(quiet.disconnected());
});

test("a port that closed after the hello fails the pick without sending it", async () => {
  const { fake, sent } = port(() => [welcome(["element.pick"]), HANG_UP]);

  assert.equal(await failure(deliver(fake, ME, PICK)), "extension.core_unreachable");
  assert.equal(sent.length, 1);
});

test("failures are reported by code after a hello, and quietly dropped without a core", async () => {
  const told = core(undefined, []);
  const gone = port(() => undefined);

  await report(told.fake, ME, "extension.capture_failed");
  await report(gone.fake, ME, "extension.capture_failed");

  assert.deepEqual(told.sent.at(-1), { id: 2, type: "error", code: "extension.capture_failed", retryable: false });
  assert.ok(told.disconnected());
  assert.equal(gone.sent.length, 1);
  assert.ok(gone.disconnected());
});

test("the extension names its build from its manifest", () => {
  assert.deepEqual(app({ version: "0.1.0", version_name: "0.1.0 (1a2b3c4d)" }), { ...ME, commit: "1a2b3c4d" });
  assert.deepEqual(app({ version: "0.1.0", version_name: "0.1.0 (unknown)" }), { ...ME, commit: "unknown" });
  assert.deepEqual(app({ version: "0.1.0", version_name: "0.1.0 (a b)" }), { ...ME, commit: "unknown" });
  assert.deepEqual(app({ version: "0.1.0" }), { ...ME, commit: "unknown" });
});

test("the protocol version matches the Rust one", () => {
  const rust = readFileSync(new URL("../../../crates/protocol/src/message.rs", import.meta.url), "utf8");

  assert.equal(rust.match(/pub const PROTOCOL: u32 = (\d+);/)?.[1], String(PROTOCOL));
});
