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

/** Starts a fake server that streams `tokens` tokens with `paceMs` between messages, and opens the page on it. */
export async function start(tokens: number, paceMs: number): Promise<Harness> {
  const dir = mkdtempSync(join(tmpdir(), "weacon-web-"));
  const server = spawn(
    "cargo",
    ["run", "--locked", "--quiet", "-p", "weacon-testing", "--bin", "fake-server", "--", dir, String(tokens), String(paceMs)],
    { cwd: REPO, stdio: ["ignore", "pipe", "inherit"] },
  );
  let browser: Browser | undefined;
  const stop = async () => {
    await browser?.close();
    server.kill();
    rmSync(dir, { recursive: true, force: true });
  };
  try {
    const url = await firstLine(server);
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

async function firstLine(child: ChildProcess): Promise<string> {
  if (child.stdout === null) {
    throw new Error("the fake server has no stdout");
  }
  for await (const line of createInterface({ input: child.stdout })) {
    return line;
  }
  throw new Error("the fake server exited before it listened");
}
