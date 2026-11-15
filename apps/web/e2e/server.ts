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
  /** The notices waiting, each as its file holds it, which it takes out of the outbox. */
  notices(): unknown[];
  /** The server's address, for another browser's pages. */
  url: string;
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
  const read = (name: string) => JSON.parse(readFileSync(join(outbox, name), "utf8")) as { to: string; code?: string; notice?: unknown };
  const codes = () => letters().filter((name) => read(name).code !== undefined);
  return {
    page,
    admin: async (command, ...args) => {
      await promisify(execFile)("cargo", cargo(command, "--db", db, ...args), { cwd: REPO });
    },
    code: async (email) => {
      await waitFor(() => codes().length > 0);
      const name = codes()[0] ?? "";
      const letter = read(name);
      rmSync(join(outbox, name));
      if (letter.to !== email || letter.code === undefined) {
        throw new Error("the letter is for someone else");
      }
      return letter.code;
    },
    letters: () => letters().length,
    notices: () =>
      letters()
        .filter((name) => read(name).code === undefined)
        .map((name) => {
          const letter = read(name);
          rmSync(join(outbox, name));
          return letter;
        }),
    url: page.url(),
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
