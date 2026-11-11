import assert from "node:assert/strict";
import { type ChildProcess, spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { once } from "node:events";
import { test } from "node:test";

import { firstLine, start } from "./harness.ts";

// Each child is read as soon as it starts, as `start` does.
const child = (script: string) => spawn(process.execPath, ["-e", script], { stdio: ["ignore", "pipe", "ignore"] });

test("a child that never prints a line fails its start instead of hanging", async () => {
  const silent = child("setInterval(() => {}, 1000)");
  try {
    await assert.rejects(firstLine(silent, 100), /didn't start in time/);
  } finally {
    silent.kill();
  }
  assert.equal(await firstLine(child("console.log('ws://127.0.0.1:1/')"), 10_000), "ws://127.0.0.1:1/");
});

test("a server that never starts is stopped and its folder removed", async () => {
  let server: ChildProcess | undefined;
  let folder = "";
  const silent = (dir: string) => {
    folder = dir;
    server = child("setInterval(() => {}, 1000)");
    return server;
  };

  await assert.rejects(start(silent, 100), /didn't start in time/);
  if (server === undefined) {
    throw new Error("no server was started");
  }
  if (server.exitCode === null && server.signalCode === null) {
    await once(server, "exit");
  }
  assert.equal(server.signalCode, "SIGTERM");
  assert.equal(existsSync(folder), false);
});
