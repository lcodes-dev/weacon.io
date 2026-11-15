// Starts the real server, with the built web app inside it, for the shell's end-to-end tests.

import { execFile, spawn } from "node:child_process";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { promisify } from "node:util";

import type { Page } from "playwright-core";

import { REPO, STARTUP_MS, open, waitFor } from "./harness.ts";

/** A page on the server's own origin. */
export type Served = {
  page: Page;
  /** Runs one of the server's admin commands against its database, such as `add-account`. */
  admin(command: string, ...args: string[]): Promise<void>;
  /** The code in the next mail the server printed, which must be for `email`. */
  code(email: string): Promise<string>;
  /** How many printed mails no `code` has taken. */
  letters(): number;
  /** The server's address, for another browser's pages. */
  url: string;
  close(): Promise<void>;
};

const cargo = (...args: string[]) => ["run", "--locked", "--quiet", "-p", "weacon-server", "--", ...args];

/** Starts the server in a new folder and opens a page on it; on any failure, stops it and removes the folder. */
export async function serve(): Promise<Served> {
  const dir = mkdtempSync(join(tmpdir(), "weacon-shell-"));
  const db = join(dir, "db");
  const child = spawn("cargo", cargo("serve", "--listen", "127.0.0.1:0", "--db", db, "--secret", join(dir, "key")), {
    cwd: REPO,
    stdio: ["ignore", "pipe", "inherit"],
  });
  const records: LogLine[] = [];
  if (child.stdout !== null) {
    createInterface({ input: child.stdout }).on("line", (line) => records.push(parsed(line)));
  }
  const listening = () => records.find((record) => record.msg === "listening")?.address;
  const ready = async () => {
    await waitFor(() => listening() !== undefined || child.exitCode !== null || child.signalCode !== null, STARTUP_MS);
    const address = listening();
    if (address === undefined) {
      throw new Error("the server exited before it listened");
    }
    return address;
  };
  const { page, stop } = await open(dir, child, ready, (address) => `http://${address}/`);
  let taken = 0;
  const letters = () => records.filter((record) => record.msg === "mail printed").slice(taken);
  return {
    page,
    admin: async (command, ...args) => {
      await promisify(execFile)("cargo", cargo(command, "--db", db, ...args), { cwd: REPO });
    },
    code: async (email) => {
      await waitFor(() => letters().length > 0);
      const [mail] = letters();
      taken += 1;
      const code = /\b[0-9A-Z]{4}-[0-9A-Z]{4}\b/.exec(mail?.text ?? "")?.[0];
      if (mail?.to !== email || code === undefined) {
        throw new Error("the mail is for someone else, or holds no code");
      }
      return code;
    },
    letters: () => letters().length,
    url: page.url(),
    close: stop,
  };
}

/** The fields of the server's log lines that these tests read. */
type LogLine = { msg?: string; address?: string; to?: string; text?: string };

/** One of the server's log lines, or nothing from a line that isn't one. */
function parsed(line: string): LogLine {
  try {
    return JSON.parse(line) as LogLine;
  } catch {
    return {};
  }
}

export const EMAIL = "ada@example.com";

/** Makes an account for `EMAIL` and signs the page in with the code sent to it. */
export async function signIn(served: Served): Promise<void> {
  await served.admin("add-account", "--email", EMAIL);
  await signInAgain(served);
}

/** Signs in as the account `signIn` added, from the sign-in page. */
export async function signInAgain(served: Served): Promise<void> {
  await signInAs(served, served.page, EMAIL);
}

/** Signs `page` in as `email`, from the sign-in page. */
export async function signInAs(served: Served, page: Page, email: string): Promise<void> {
  await page.getByLabel("Email").fill(email);
  await page.getByRole("button", { name: "Send code" }).click();
  await page.getByLabel("Code").fill(await served.code(email));
  await page.getByRole("button", { name: "Sign in" }).click();
}
