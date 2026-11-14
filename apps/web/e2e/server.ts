// Starts the real server, with the built web app inside it, for the shell's end-to-end tests.

import { execFile, spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";

import type { Page } from "playwright-core";

import { REPO, firstLine, open, waitFor } from "./harness.ts";

/** A page on the server's own origin. */
export type Served = {
  page: Page;
  /** Runs one of the server's admin commands against its database, such as `add-account`. */
  admin(command: string, ...args: string[]): Promise<void>;
  /** The code in the one letter waiting for `email`, which it takes out of the outbox. */
  code(email: string): Promise<string>;
  /** How many letters wait in the outbox. */
  letters(): number;
  close(): Promise<void>;
};

const cargo = (...args: string[]) => ["run", "--locked", "--quiet", "-p", "weacon-server", "--", ...args];

/** Starts the server in a new folder and opens a page on it; on any failure, stops it and removes the folder. */
export async function serve(): Promise<Served> {
  const dir = mkdtempSync(join(tmpdir(), "weacon-shell-"));
  const db = join(dir, "db");
  const outbox = join(dir, "outbox");
  mkdirSync(outbox);
  const child = spawn("cargo", cargo("serve", "--listen", "127.0.0.1:0", "--db", db, "--outbox", outbox, "--secret", join(dir, "key")), {
    cwd: REPO,
    stdio: ["ignore", "pipe", "inherit"],
  });
  const { page, stop } = await open(dir, child, (child) => firstLine(child, undefined, listening), (address) => `http://${address}/`);
  const letters = () => readdirSync(outbox).filter((name) => name.endsWith(".json"));
  return {
    page,
    admin: async (command, ...args) => {
      await promisify(execFile)("cargo", cargo(command, "--db", db, ...args), { cwd: REPO });
    },
    code: async (email) => {
      await waitFor(() => letters().length > 0);
      const path = join(outbox, letters()[0] ?? "");
      const letter = JSON.parse(readFileSync(path, "utf8")) as { to: string; code: string };
      rmSync(path);
      if (letter.to !== email) {
        throw new Error("the letter is for someone else");
      }
      return letter.code;
    },
    letters: () => letters().length,
    close: stop,
  };
}

/** The address in the server's `listening` log line. */
function listening(line: string): string | undefined {
  try {
    const record = JSON.parse(line) as { msg?: string; address?: string };
    return record.msg === "listening" ? record.address : undefined;
  } catch {
    return undefined;
  }
}
