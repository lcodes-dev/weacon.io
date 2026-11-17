import assert from "node:assert/strict";
import { type ChildProcess, spawn } from "node:child_process";
import { existsSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { once } from "node:events";
import { test } from "node:test";

import { firstLine, open, start, waitFor } from "./harness.ts";

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

  await assert.rejects(start(silent, { startupMs: 100 }), /didn't start in time/);
  if (server === undefined) {
    throw new Error("no server was started");
  }
  if (server.exitCode === null && server.signalCode === null) {
    await once(server, "exit");
  }
  assert.equal(server.signalCode, "SIGTERM");
  assert.equal(existsSync(folder), false);
});

test("a route still answering when its test stops fails nothing after it", async (t) => {
  const dir = mkdtempSync(join(tmpdir(), "weacon-harness-"));
  const { page, stop } = await open(dir, child("console.log('ready'); setInterval(() => {}, 1000)"), (server) => firstLine(server, 10_000), () => "about:blank");
  // Stopping twice is harmless, and a step that fails before the stop below leaves nothing running.
  t.after(stop);
  let answering = false;
  await page.route("**/slow", async (route) => {
    answering = true;
    // Asks the page something before it answers, which fails once the page is closed.
    await page.evaluate(() => new Promise((resolve) => setTimeout(resolve, 60_000)));
    await route.fulfill({ status: 200, body: "late" });
  });
  void page.evaluate(() => void fetch("http://weacon.invalid/slow").catch(() => undefined)).catch(() => undefined);
  await waitFor(() => answering);

  // The runner fails a test that leaves a rejection no one handles.
  await stop();
});
