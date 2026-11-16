import assert from "node:assert/strict";
import { test } from "node:test";

import type { Envelope, MachineSettings } from "../protocol.ts";
import { NO_ANSWER, RECONNECTING } from "./ask-machine.ts";
import { fake } from "./fake-socket.ts";
import { agents, change, current, repos } from "./remote-settings.ts";

const PLACE = { machine: "mc-1", workspace: "w-1" };
const SETTINGS: MachineSettings = {
  reports: true,
  access: "ask_first",
  sign_ins: [{ agent: "claude", state: "signed_in" }],
};

function request(envelope: Envelope | undefined): string {
  assert.ok(envelope !== undefined && "request" in envelope);
  return envelope.request;
}

test("a query names the machine and the socket's workspace, and takes the answer to its own request", async () => {
  const core = fake();
  const asked = current(core.socket, "mc-1", core.later);
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
  const changed = change(core.socket, "mc-1", { kind: "set_access", level: "full" }, core.later);
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
  const waiting = current(silent.socket, "mc-1", silent.later);
  silent.expire();
  const newer = fake();
  const odd = current(newer.socket, "mc-1", newer.later);
  newer.deliver({ id: 3, type: "machine_settings_answer", request: request(newer.sent[0]), ...PLACE, answer: { kind: "later" } as never });
  const closed = fake({ send: "throws" });
  const dropped = fake({ send: "dropped" });

  assert.deepEqual(await waiting, { ok: false, code: NO_ANSWER });
  assert.deepEqual(await odd, { ok: false, code: "answer.malformed" });
  assert.deepEqual(await current(closed.socket, "mc-1", closed.later), { ok: false, code: "connection.refused" });
  assert.deepEqual(await change(dropped.socket, "mc-1", { kind: "set_reports", send: true }, dropped.later), {
    ok: false,
    code: RECONNECTING,
  });
  for (const core of [silent, newer, closed, dropped]) {
    assert.equal(core.listeners(), 0);
  }
});

test("an agents query asks which agents are signed in for the workspace, and another kind of answer is malformed", async () => {
  const core = fake();
  const asked = agents(core.socket, "mc-1", core.later);
  const [query] = core.sent;
  core.deliver({ id: 2, type: "machine_settings_answer", request: request(query), ...PLACE, answer: { kind: "agents", agents: ["codex"] } });
  const odd = fake();
  const wrong = agents(odd.socket, "mc-1", odd.later);
  odd.deliver({ id: 2, type: "machine_settings_answer", request: request(odd.sent[0]), ...PLACE, answer: { kind: "repos", repos: [] } });

  assert.deepEqual(await asked, { ok: true, value: ["codex"] });
  assert.deepEqual({ ...query, request: "r" }, { id: 11, type: "machine_settings_query", request: "r", ...PLACE, query: { kind: "agents" } });
  assert.deepEqual(await wrong, { ok: false, code: "answer.malformed" });
});

test("a repositories query asks for the workspace's repositories and takes their ids, and another kind of answer is malformed", async () => {
  const core = fake();
  const asked = repos(core.socket, "mc-1", core.later);
  const [query] = core.sent;
  core.deliver({ id: 2, type: "machine_settings_answer", request: request(query), ...PLACE, answer: { kind: "repos", repos: ["api", "web"] } });
  const odd = fake();
  const wrong = repos(odd.socket, "mc-1", odd.later);
  odd.deliver({ id: 2, type: "machine_settings_answer", request: request(odd.sent[0]), ...PLACE, answer: { kind: "current", ...SETTINGS } });

  assert.deepEqual(await asked, { ok: true, value: ["api", "web"] });
  assert.deepEqual({ ...query, request: "r" }, { id: 11, type: "machine_settings_query", request: "r", ...PLACE, query: { kind: "repos" } });
  assert.deepEqual(await wrong, { ok: false, code: "answer.malformed" });
});
