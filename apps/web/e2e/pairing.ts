// Pairing, and a core that answers, for the end-to-end tests of Settings.

import assert from "node:assert/strict";

import type { Page } from "playwright-core";

import { PROTOCOL } from "../../../tooling/ts/version.ts";
import type { App, ChatEntry, Envelope, MachineSettings, OutsideSession, PairClaim, PairClaimed, PairStart, PairStarted, SessionsAnswerKind, Symbol as CodeSymbol } from "../src/protocol.ts";

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
  await confirm(page, code);
  return claim(page, { pairing, secret });
}

/** Confirms the pairing waiting under `code` from Settings, as a person would. */
export async function confirm(page: Page, code: string): Promise<void> {
  await page.getByLabel("Code").fill(code);
  await page.getByRole("button", { name: "Look up" }).click();
  const confirmed = page.waitForResponse((response) => response.url().endsWith("/api/machines/pair/confirm") && response.ok());
  await page.getByRole("button", { name: "Confirm" }).click();
  await confirmed;
}

/** The build the test cores say they are. */
export const BUILD: App = { name: "weacon-core", version: "0.0.0", commit: "0000000" };

/** A core's connection to the server, which says `hello` as `app` offering `capabilities` once open. */
export function dial(page: Page, token: string, app: App, capabilities: string[]): WebSocket {
  const url = new URL("/api/machines/connect", page.url());
  url.protocol = "ws:";
  // Node's WebSocket takes headers, as the core's own client sends its bearer token.
  const socket = new WebSocket(url, { headers: { authorization: `Bearer ${token}` } } as unknown as string[]);
  socket.onopen = () => socket.send(JSON.stringify({ id: 1, type: "hello", protocol: PROTOCOL, app, capabilities } satisfies Envelope));
  return socket;
}

/** The sessions a person ran outside Weacon, as a core reads them: its list, and the one session's events so far. */
export type Outside = { sessions: OutsideSession[]; events: ChatEntry[] };

/**
 * A machine's core, connected to the server, that keeps `settings` and answers for them, and
 * counts the queries; once told to refuse, it refuses changes as a core does a stranger's.
 * Given `outside`, it also answers for those sessions, as they are when asked, and given
 * `symbols`, it finds those whose name holds every word asked.
 */
export async function core(
  page: Page,
  token: string,
  settings: MachineSettings,
  outside?: Outside,
  symbols?: CodeSymbol[],
): Promise<{ settings: MachineSettings; queries: number; set(settings: MachineSettings): void; refuse(): void; close(): void }> {
  const socket = dial(page, token, BUILD, ["machine.connect", "machine.settings", ...(outside === undefined ? [] : ["sessions.read"]), ...(symbols === undefined ? [] : ["symbols.search"])]);
  const held = { settings, refusing: false, queries: 0 };
  const send = (envelope: Envelope) => socket.send(JSON.stringify(envelope));
  await new Promise<void>((resolve, reject) => {
    socket.onerror = () => reject(new Error("the core couldn't connect"));
    socket.onmessage = ({ data }) => {
      const envelope = JSON.parse(String(data)) as Envelope;
      if (envelope.type === "welcome") {
        resolve();
      } else if (envelope.type === "sessions_query" && outside !== undefined) {
        const { query } = envelope;
        const answer: SessionsAnswerKind =
          query.kind === "list" ? { kind: "list", sessions: outside.sessions } : { kind: "opened", events: outside.events.filter(({ seq }) => seq > query.after), more: false };
        send({ id: envelope.id, type: "sessions_answer", request: envelope.request, machine: envelope.machine, answer });
      } else if (envelope.type === "symbols_query" && symbols !== undefined && envelope.query.kind === "search") {
        const words = envelope.query.words.toLowerCase().split(/\s+/).filter((word) => word !== "");
        const found = symbols.filter(({ name }) => words.every((word) => name.toLowerCase().includes(word)));
        const { request, machine, workspace } = envelope;
        send({ id: envelope.id, type: "symbols_answer", request, machine, workspace, answer: { kind: "found", symbols: found, more: false } });
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
