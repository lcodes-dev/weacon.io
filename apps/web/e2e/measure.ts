// `npm run measure`: streams the whole 100,000-token session into the built page in Chromium
// and prints frame gaps, memory, DOM size and bundle size as JSON, for the PR that changes them.

import { readFileSync } from "node:fs";
import { gzipSync } from "node:zlib";

import { start } from "./harness.ts";

// How long without a new row means the run is over; the fake server keeps the connection open.
const QUIET_MS = 2_000;

const harness = await start(100_000, 1);
try {
  const { page } = harness;
  await page.evaluate(() => {
    const gaps: number[] = [];
    let last = performance.now();
    const tick = (now: number) => {
      gaps.push(now - last);
      last = now;
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
    Object.assign(window, { gaps });
  });
  const began = Date.now();
  await page.waitForFunction(
    (quiet) => {
      const state = window as unknown as { seen?: number; since?: number };
      const rows = document.querySelectorAll(".row").length;
      if (rows !== state.seen) {
        state.seen = rows;
        state.since = performance.now();
      }
      return rows > 0 && performance.now() - (state.since ?? 0) > quiet;
    },
    QUIET_MS,
    { timeout: 300_000, polling: 100 },
  );
  const streamedMs = Date.now() - began - QUIET_MS;
  const gaps = (await page.evaluate(() => (window as unknown as { gaps: number[] }).gaps)).sort((a, b) => a - b);
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("HeapProfiler.collectGarbage");
  await cdp.send("Performance.enable");
  const { metrics } = await cdp.send("Performance.getMetrics");
  const metric = (name: string) => metrics.find((entry) => entry.name === name)?.value ?? 0;
  const at = (share: number) => Math.round(gaps[Math.min(gaps.length - 1, Math.floor(gaps.length * share))] ?? 0);
  const script = readFileSync(new URL("../dist/app.js", import.meta.url));
  const result = {
    streamedMs,
    rows: await page.evaluate(() => document.querySelectorAll(".row").length),
    frames: gaps.length,
    frameGapMs: { median: at(0.5), p95: at(0.95), p99: at(0.99), longest: Math.round(gaps.at(-1) ?? 0) },
    jsHeapMiB: Math.round(metric("JSHeapUsedSize") / 2 ** 20),
    domNodes: metric("Nodes"),
    bundle: { appJsBytes: script.length, appJsGzipBytes: gzipSync(script).length },
  };
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
} finally {
  await harness.close();
}
