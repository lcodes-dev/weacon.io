// Starts the fake server and a browser on the built app, for the end-to-end tests.

import { type ChildProcess, spawn } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";

import { type Browser, type Page, chromium } from "playwright-core";

export const REPO = fileURLToPath(new URL("../../..", import.meta.url));
const PAGE = new URL("../dist/index.html", import.meta.url);

/** A page open on a fake server. */
export type Harness = {
  page: Page;
  /** What the page has sent the server after its hello, parsed. */
  received(): unknown[];
  close(): Promise<void>;
};

/** The fake server streaming `tokens` tokens with `paceMs` between messages, keeping its files in a folder. */
export function fakeServer(tokens: number, paceMs: number): (dir: string) => ChildProcess {
  return (dir) =>
    spawn(
      "cargo",
      ["run", "--locked", "--quiet", "-p", "weacon-testing", "--bin", "fake-server", "--", dir, String(tokens), String(paceMs)],
      { cwd: REPO, stdio: ["ignore", "pipe", "inherit"] },
    );
}

/**
 * Starts `server` in a new folder and opens the page on it, with `query` after its address; on
 * any failure, stops it and removes the folder.
 */
export async function start(
  server: (dir: string) => ChildProcess,
  { startupMs = STARTUP_MS, query = "" }: { startupMs?: number; query?: string } = {},
): Promise<Harness> {
  const dir = mkdtempSync(join(tmpdir(), "weacon-web-"));
  const { page, stop } = await open(dir, server(dir), (child) => firstLine(child, startupMs), (url) => `${PAGE.href}?server=${encodeURIComponent(url)}${query}`);
  return {
    page,
    received: () =>
      readFileSync(join(dir, "received.jsonl"), "utf8")
        .split("\n")
        .filter((line) => line !== "")
        .map((line) => JSON.parse(line) as unknown),
    close: stop,
  };
}

/**
 * Opens a browser on `href` of what `ready` reads from `child`, which runs in `dir`. `stop` closes
 * the browser, stops `child` and removes `dir`, as happens on any failure.
 */
export async function open(
  dir: string,
  child: ChildProcess,
  ready: (child: ChildProcess) => Promise<string>,
  href: (found: string) => string,
): Promise<{ page: Page; stop(): Promise<void> }> {
  let browser: Browser | undefined;
  const stop = async () => {
    await browser?.close();
    child.kill();
    rmSync(dir, { recursive: true, force: true });
  };
  try {
    const found = await ready(child);
    browser = await chromium.launch();
    const page = await browser.newPage();
    await page.goto(href(found));
    return { page, stop };
  } catch (error) {
    await stop();
    throw error;
  }
}

// Long enough for cargo to build the server first.
export const STARTUP_MS = 300_000;

/**
 * What `pick` finds in the first of `child`'s lines it accepts, at first any line; or a failure
 * once `deadlineMs` passes without one, so `open` cleans up.
 */
export async function firstLine(
  child: ChildProcess,
  deadlineMs = STARTUP_MS,
  pick: (line: string) => string | undefined = (line) => line,
): Promise<string> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const late = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error("the server didn't start in time")), deadlineMs);
  });
  try {
    return await Promise.race([readLine(child, pick), late]);
  } finally {
    clearTimeout(timer);
  }
}

async function readLine(child: ChildProcess, pick: (line: string) => string | undefined): Promise<string> {
  if (child.stdout === null) {
    throw new Error("the server has no stdout");
  }
  for await (const line of createInterface({ input: child.stdout })) {
    const found = pick(line);
    if (found !== undefined) {
      return found;
    }
  }
  throw new Error("the server exited before it listened");
}

/** Polls until `done`; fails after `deadlineMs`. */
export async function waitFor(done: () => boolean, deadlineMs = 10_000): Promise<void> {
  const end = Date.now() + deadlineMs;
  while (!done()) {
    if (Date.now() > end) {
      throw new Error("gave up waiting");
    }
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}
