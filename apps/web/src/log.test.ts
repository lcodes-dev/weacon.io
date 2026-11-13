import assert from "node:assert/strict";
import { test } from "node:test";

import { Log } from "./log.ts";
import type { Envelope } from "./protocol.ts";

function attached(forwards: boolean): { log: Log; sent: Envelope[] } {
  const log = new Log();
  const sent: Envelope[] = [];
  log.attach((envelope) => sent.push(envelope), forwards);
  return { log, sent };
}

test("records wait for the server, then go to it in order", () => {
  const log = new Log();
  log.info("app started");
  log.report("web.crashed");
  const sent: Envelope[] = [];
  log.attach((envelope) => sent.push(envelope), true);
  log.warn("connection lost", { code: "connection.lost" });
  log.detach();
  log.warn("connection refused");

  assert.deepEqual(sent, [
    { id: 0, type: "log", level: "info", msg: "app started" },
    { id: 0, type: "error", code: "web.crashed", retryable: false },
    { id: 0, type: "log", level: "warn", msg: "connection lost", code: "connection.lost" },
  ]);
});

test("a server that doesn't forward logs still gets error reports", () => {
  const { log, sent } = attached(false);
  log.info("app started");
  log.report("web.crashed");

  assert.deepEqual(sent, [{ id: 0, type: "error", code: "web.crashed", retryable: false }]);
});

test("only the newest records wait", () => {
  const log = new Log();
  for (let at = 0; at < 250; at += 1) {
    log.report("web.crashed");
  }
  log.info("app started");
  const sent: Envelope[] = [];
  log.attach((envelope) => sent.push(envelope), true);

  assert.equal(sent.length, 200);
  assert.deepEqual(sent.at(-1), { id: 0, type: "log", level: "info", msg: "app started" });
});
