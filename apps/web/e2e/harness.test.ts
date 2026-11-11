import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { test } from "node:test";

import { firstLine } from "./harness.ts";

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
