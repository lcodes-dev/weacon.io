import assert from "node:assert/strict";
import { test } from "node:test";

import type { Later } from "../connection.ts";
import type { Incoming } from "../incoming.ts";
import type { Envelope, MachineSettings } from "../protocol.ts";
import { type Link, NO_ANSWER, RECONNECTING, change, current } from "./link.ts";

const PLACE = { machine: "mc-1", workspace: "w-1" };
const SETTINGS: MachineSettings = {
  reports: true,
  access: "ask_first",
  team: false,
  sign_ins: [{ agent: "claude", state: "signed_in" }],
};

/** A link to the open workspace `w-1` whose core the test plays, and a clock it runs by hand. */
function fake(how: { send?: "throws" | "dropped" } = {}) {
  const sent: Envelope[] = [];
  const handles = new Set<(message: Incoming) => void>();
  const timers: Array<() => void> = [];
  let ids = 10;
  const link: Link = {
    workspace: "w-1",
    send: (envelope) => {
      if (how.send === "throws") throw new Error("closed");
      if (how.send === "dropped") return false;
      sent.push(envelope);
      return true;
    },
    subscribe: (handle) => {
      handles.add(handle);
      return () => handles.delete(handle);
    },
    nextId: () => ++ids,
  };
  const later: Later = (run) => {
    timers.push(run);
  };
  return {
    link,
    later,
    sent,
    deliver: (message: Incoming) => {
      for (const handle of [...handles]) handle(message);
    },
    listeners: () => handles.size,
    expire: () => {
      for (const run of timers) run();
    },
  };
}

function request(envelope: Envelope | undefined): string {
  assert.ok(envelope !== undefined && "request" in envelope);
  return envelope.request;
}

test("a query names the machine and the link's workspace, and takes the answer to its own request", async () => {
  const core = fake();
  const asked = current(core.link, "mc-1", core.later);
  const [query] = core.sent;
  core.deliver({ id: 1, type: "machine_settings_answer", request: "someone-else", ...PLACE, answer: { kind: "current", ...SETTINGS, reports: false } });
  core.deliver({ id: 2, type: "machine_settings_answer", request: request(query), ...PLACE, answer: { kind: "current", ...SETTINGS } });
  core.expire();

  assert.deepEqual(await asked, { ok: true, value: SETTINGS });
  assert.deepEqual({ ...query, request: "r" }, { id: 11, type: "machine_settings_query", request: "r", ...PLACE, query: { kind: "current" } });
  assert.equal(core.listeners(), 0);
});

test("a command carries its action, and an error for its envelope answers it", async () => {
  const core = fake();
  const changed = change(core.link, "mc-1", { kind: "set_access", level: "full" }, core.later);
  const [command] = core.sent;
  core.deliver({ id: 99, type: "error", code: "settings.unrelated", retryable: false });
  core.deliver({ id: command?.id ?? 0, type: "error", code: "settings.not_owner", retryable: false });

  assert.deepEqual(await changed, { ok: false, code: "settings.not_owner" });
  assert.ok(command?.type === "machine_settings_command");
  assert.deepEqual(command.action, { kind: "set_access", level: "full" });
  assert.deepEqual({ machine: command.machine, workspace: command.workspace }, PLACE);
  assert.equal(core.listeners(), 0);
});

test("no answer in time, a newer answer, a closed socket or a reconnecting one each end the request", async () => {
  const silent = fake();
  const waiting = current(silent.link, "mc-1", silent.later);
  silent.expire();
  const newer = fake();
  const odd = current(newer.link, "mc-1", newer.later);
  newer.deliver({ id: 3, type: "machine_settings_answer", request: request(newer.sent[0]), ...PLACE, answer: { kind: "later" } as never });
  const closed = fake({ send: "throws" });
  const dropped = fake({ send: "dropped" });

  assert.deepEqual(await waiting, { ok: false, code: NO_ANSWER });
  assert.deepEqual(await odd, { ok: false, code: "answer.malformed" });
  assert.deepEqual(await current(closed.link, "mc-1", closed.later), { ok: false, code: "connection.refused" });
  assert.deepEqual(await change(dropped.link, "mc-1", { kind: "set_reports", send: true }, dropped.later), {
    ok: false,
    code: RECONNECTING,
  });
  for (const core of [silent, newer, closed, dropped]) {
    assert.equal(core.listeners(), 0);
  }
});
