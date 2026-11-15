// Pairing, and a core that answers, for the end-to-end tests of Settings.

import assert from "node:assert/strict";

import type { Page } from "playwright-core";

import { PROTOCOL } from "../../../tooling/ts/version.ts";
import type { Envelope, MachineSettings, PairClaim, PairClaimed, PairStart, PairStarted } from "../src/protocol.ts";

// The core's own test key, and the fingerprint the core prints for it.
export const KEY = "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a";
export const FINGERPRINT = "300c 9c96 03b9 2a4b 39ed 3958 bf92 4011";

/** What a core sends to start pairing, as `weacon pair` does. */
export async function startPairing(page: Page): Promise<PairStarted> {
  const start: PairStart = { name: "Studio", os: "linux", key: KEY };
  const started = await page.request.post(new URL("/api/machines/pair", page.url()).href, { data: start });
  assert.equal(started.status(), 201);
  return (await started.json()) as PairStarted;
}

/** The key this browser pinned for `machine`, read straight from its IndexedDB. */
export function pinned(page: Page, machine: string): Promise<unknown> {
  return page.evaluate(
    (machine) =>
      new Promise((resolve, reject) => {
        const open = indexedDB.open("weacon", 1);
        open.onerror = () => reject(open.error);
        open.onsuccess = () => {
          const read = open.result.transaction("pins").objectStore("pins").get(machine);
          read.onsuccess = () => {
            open.result.close();
            resolve(read.result);
          };
        };
      }),
    machine,
  );
}

/** Sets the key this browser pinned for `machine`, or forgets it, straight in its IndexedDB, as another browser's store would hold it. */
export function setPin(page: Page, machine: string, key: string | undefined): Promise<unknown> {
  return page.evaluate(
    ([machine, key]) =>
      new Promise((resolve, reject) => {
        const open = indexedDB.open("weacon", 1);
        open.onerror = () => reject(open.error);
        open.onsuccess = () => {
          const pins = open.result.transaction("pins", "readwrite").objectStore("pins");
          const set = key === undefined ? pins.delete(machine) : pins.put(key, machine);
          set.onsuccess = () => {
            open.result.close();
            resolve(undefined);
          };
        };
      }),
    [machine, key] as const,
  );
}

/** Claims a confirmed pairing as its core does, and gives the machine and its token. */
export async function claim(page: Page, claim: PairClaim): Promise<{ machine: string; token: string }> {
  const claimed = await page.request.post(new URL("/api/machines/pair/claim", page.url()).href, { data: claim });
  const paired = (await claimed.json()) as PairClaimed;
  assert.ok(paired.state === "paired");
  return { machine: paired.machine, token: paired.token };
}

/** Pairs a machine through the page as a person would, then claims it; gives the machine and the core's token. */
export async function pair(page: Page): Promise<{ machine: string; token: string }> {
  const { pairing, code, secret } = await startPairing(page);
  await page.getByLabel("Code").fill(code);
  await page.getByRole("button", { name: "Look up" }).click();
  const confirmed = page.waitForResponse((response) => response.url().endsWith("/api/machines/pair/confirm") && response.ok());
  await page.getByRole("button", { name: "Confirm" }).click();
  await confirmed;
  return claim(page, { pairing, secret });
}

/**
 * A machine's core, connected to the server, that keeps `settings` and answers for them, and
 * counts the queries; once told to refuse, it refuses changes as a core does a stranger's.
 */
export async function core(
  page: Page,
  token: string,
  settings: MachineSettings,
): Promise<{ settings: MachineSettings; queries: number; set(settings: MachineSettings): void; refuse(): void; close(): void }> {
  const url = new URL("/api/machines/connect", page.url());
  url.protocol = "ws:";
  // Node's WebSocket takes headers, as the core's own client sends its bearer token.
  const socket = new WebSocket(url, { headers: { authorization: `Bearer ${token}` } } as unknown as string[]);
  const held = { settings, refusing: false, queries: 0 };
  const send = (envelope: Envelope) => socket.send(JSON.stringify(envelope));
  await new Promise<void>((resolve, reject) => {
    socket.onerror = () => reject(new Error("the core couldn't connect"));
    socket.onopen = () => {
      send({ id: 1, type: "hello", protocol: PROTOCOL, app: { name: "weacon-core", version: "0.0.0", commit: "0000000" }, capabilities: ["machine.connect", "machine.settings"] });
    };
    socket.onmessage = ({ data }) => {
      const envelope = JSON.parse(String(data)) as Envelope;
      if (envelope.type === "welcome") {
        resolve();
      } else if (envelope.type === "machine_settings_command" && held.refusing) {
        send({ id: envelope.id, type: "error", code: "settings.not_owner", retryable: false });
        return;
      } else if (envelope.type === "machine_settings_command") {
        const { action } = envelope;
        if (action.kind === "set_reports") {
          held.settings = { ...held.settings, reports: action.send };
        } else if (action.kind === "set_access") {
          held.settings = { ...held.settings, access: action.level, team: action.team ?? held.settings.team };
        }
      }
      if (envelope.type === "machine_settings_query") {
        held.queries += 1;
      }
      if (envelope.type === "machine_settings_query" || envelope.type === "machine_settings_command") {
        const { request, machine, workspace } = envelope;
        send({ id: envelope.id, type: "machine_settings_answer", request, machine, workspace, answer: { kind: "current", ...held.settings } });
      }
    };
  });
  return {
    get settings() {
      return held.settings;
    },
    get queries() {
      return held.queries;
    },
    set: (next) => {
      held.settings = next;
    },
    refuse: () => {
      held.refusing = true;
    },
    close: () => socket.close(),
  };
}
