// Starts the fake server and a browser on the built app, for the end-to-end tests.

import { type ChildProcess, spawn } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";

import { type Browser, type Page, chromium } from "playwright-core";

const REPO = fileURLToPath(new URL("../../..", import.meta.url));
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

/** Starts `server` in a new folder and opens the page on it; on any failure, stops it and removes the folder. */
export async function start(server: (dir: string) => ChildProcess, startupMs = STARTUP_MS): Promise<Harness> {
  const dir = mkdtempSync(join(tmpdir(), "weacon-web-"));
  const child = server(dir);
  let browser: Browser | undefined;
  const stop = async () => {
    await browser?.close();
    child.kill();
    rmSync(dir, { recursive: true, force: true });
  };
  try {
    const url = await firstLine(child, startupMs);
    browser = await chromium.launch();
    const page = await browser.newPage();
    await page.goto(`${PAGE.href}?server=${encodeURIComponent(url)}`);
    return {
      page,
      received: () =>
        readFileSync(join(dir, "received.jsonl"), "utf8")
          .split("\n")
          .filter((line) => line !== "")
          .map((line) => JSON.parse(line) as unknown),
      close: stop,
    };
  } catch (error) {
    await stop();
    throw error;
  }
}

// Long enough for cargo to build the fake server first.
const STARTUP_MS = 300_000;

/** `child`'s first line, or a failure once `deadlineMs` passes without one, so `start` cleans up. */
export async function firstLine(child: ChildProcess, deadlineMs = STARTUP_MS): Promise<string> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  const late = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error("the fake server didn't start in time")), deadlineMs);
  });
  try {
    return await Promise.race([readFirstLine(child), late]);
  } finally {
    clearTimeout(timer);
  }
}

async function readFirstLine(child: ChildProcess): Promise<string> {
  if (child.stdout === null) {
    throw new Error("the fake server has no stdout");
  }
  for await (const line of createInterface({ input: child.stdout })) {
    return line;
  }
  throw new Error("the fake server exited before it listened");
}
