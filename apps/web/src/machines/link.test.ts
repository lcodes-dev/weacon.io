import assert from "node:assert/strict";
import { test } from "node:test";

import type { Envelope, MachineSettings } from "../protocol.ts";
import { type Link, NO_ANSWER, type Wait, change, current } from "./link.ts";

const PLACE = { machine: "mc-1", workspace: "w-1" };
const SETTINGS: MachineSettings = {
  reports: true,
  access: "ask_first",
  team: false,
  sign_ins: [{ agent: "claude", state: "signed_in" }],
};

/** A link whose core the test plays, and a clock it moves by hand. */
function fake(): { link: Link; sent: Envelope[]; deliver(envelope: Envelope): void; listeners(): number; wait: Wait; expire(): void; failing(): void } {
  const sent: Envelope[] = [];
  const handles = new Set<(envelope: Envelope) => void>();
  const timers = new Set<() => void>();
  let ids = 10;
  let broken = false;
  return {
    link: {
      send: (envelope) => {
        if (broken) throw new Error("closed");
        sent.push(envelope);
      },
      subscribe: (handle) => {
        handles.add(handle);
        return () => handles.delete(handle);
      },
      nextId: () => ++ids,
    },
    sent,
    deliver: (envelope) => {
      for (const handle of [...handles]) handle(envelope);
    },
    listeners: () => handles.size,
    wait: (run) => {
      timers.add(run);
      return () => timers.delete(run);
    },
    expire: () => {
      for (const run of [...timers]) run();
    },
    failing: () => {
      broken = true;
    },
  };
}

function request(envelope: Envelope | undefined): string {
  assert.ok(envelope !== undefined && "request" in envelope);
  return envelope.request;
}

test("a query names the machine and the workspace, and takes the answer to its own request", async () => {
  const core = fake();
  const asked = current(core.link, PLACE, core.wait);
  const [query] = core.sent;
  core.deliver({ id: 1, type: "machine_settings_answer", request: "someone-else", ...PLACE, answer: { kind: "current", ...SETTINGS, reports: false } });
  core.deliver({ id: 2, type: "machine_settings_answer", request: request(query), ...PLACE, answer: { kind: "current", ...SETTINGS } });

  assert.deepEqual(await asked, { ok: true, value: SETTINGS });
  assert.equal(query?.type, "machine_settings_query");
  assert.deepEqual({ ...query, request: "r" }, { id: 11, type: "machine_settings_query", request: "r", ...PLACE, query: { kind: "current" } });
  assert.equal(core.listeners(), 0);
});

test("a command carries its action, and an error for its envelope answers it", async () => {
  const core = fake();
  const changed = change(core.link, PLACE, { kind: "set_access", level: "full" }, core.wait);
  const [command] = core.sent;
  core.deliver({ id: 99, type: "error", code: "settings.unrelated", retryable: false });
  core.deliver({ id: command?.id ?? 0, type: "error", code: "settings.not_owner", retryable: false });

  assert.deepEqual(await changed, { ok: false, code: "settings.not_owner" });
  assert.equal(command?.type, "machine_settings_command");
  assert.ok(command?.type === "machine_settings_command");
  assert.deepEqual(command.action, { kind: "set_access", level: "full" });
  assert.deepEqual({ machine: command.machine, workspace: command.workspace }, PLACE);
  assert.equal(core.listeners(), 0);
});

test("a core that never answers, a newer answer, or a closed connection each end the request", async () => {
  const silent = fake();
  const waiting = current(silent.link, PLACE, silent.wait);
  silent.expire();
  const newer = fake();
  const odd = current(newer.link, PLACE, newer.wait);
  newer.deliver({ id: 3, type: "machine_settings_answer", request: request(newer.sent[0]), ...PLACE, answer: { kind: "later" } as never });
  const closed = fake();
  closed.failing();

  assert.deepEqual(await waiting, { ok: false, code: NO_ANSWER });
  assert.deepEqual(await odd, { ok: false, code: "answer.malformed" });
  assert.deepEqual(await current(closed.link, PLACE, closed.wait), { ok: false, code: "connection.refused" });
  assert.equal(silent.listeners() + newer.listeners() + closed.listeners(), 0);
});
