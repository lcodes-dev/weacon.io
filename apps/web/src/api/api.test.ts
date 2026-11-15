import assert from "node:assert/strict";
import { test } from "node:test";

import { Api, type Fetch, MALFORMED, UNREACHABLE } from "./api.ts";

type Sent = { path: string; method: string; headers?: Record<string, string>; body?: string };

/** An `Api` whose server answers each request with the next of `answers`, recording what it was sent. */
function serve(...answers: Array<{ status: number; body?: unknown } | "unreachable">): { api: Api; sent: Sent[]; ended: () => number } {
  const sent: Sent[] = [];
  let ends = 0;
  const fetch: Fetch = (path, init) => {
    sent.push({ path, ...init });
    const answer = answers.shift();
    if (answer === undefined || answer === "unreachable") {
      return Promise.reject(new TypeError("Failed to fetch"));
    }
    return Promise.resolve({
      status: answer.status,
      json: () => ("body" in answer ? Promise.resolve(answer.body) : Promise.reject(new SyntaxError("no body"))),
    });
  };
  return { api: new Api(fetch, () => ends++), sent, ended: () => ends };
}

const ADA = { account: "acc-1", email: "ada@example.com" };

test("the session is the signed-in account, or nobody when the server asks to sign in", async () => {
  const { api, sent } = serve(
    { status: 200, body: ADA },
    { status: 401, body: { code: "sign_in.required", retryable: false } },
    { status: 401, body: { code: "sign_in.too_many", retryable: false } },
  );

  assert.deepEqual(await api.session(), { ok: true, value: ADA });
  assert.deepEqual(await api.session(), { ok: true, value: undefined });
  assert.deepEqual(await api.session(), { ok: false, code: "sign_in.too_many" });
  assert.deepEqual(sent, Array(3).fill({ path: "/api/session", method: "GET" }));
});

test("signing in sends the email, then the code with its challenge, as JSON", async () => {
  const { api, sent } = serve({ status: 202, body: { challenge: "ch-1" } }, { status: 200, body: ADA });

  assert.deepEqual(await api.startSignIn("ada@example.com"), { ok: true, value: { challenge: "ch-1" } });
  assert.deepEqual(await api.finishSignIn({ email: "ada@example.com", code: "12345678", challenge: "ch-1" }), { ok: true, value: ADA });
  const json = { "content-type": "application/json" };
  assert.deepEqual(sent, [
    { path: "/api/sign-in", method: "POST", headers: json, body: '{"email":"ada@example.com"}' },
    {
      path: "/api/sign-in/finish",
      method: "POST",
      headers: json,
      body: '{"email":"ada@example.com","code":"12345678","challenge":"ch-1"}',
    },
  ]);
});

test("signing out takes the server's empty answer", async () => {
  const { api, sent } = serve({ status: 204 }, { status: 403, body: { code: "request.cross_site", retryable: false } }, { status: 200 });

  assert.deepEqual(await api.signOut(), { ok: true, value: undefined });
  assert.deepEqual(await api.signOut(), { ok: false, code: "request.cross_site" });
  assert.deepEqual(await api.signOut(), { ok: false, code: MALFORMED });
  assert.deepEqual(sent[0], { path: "/api/sign-out", method: "POST" });
});

test("a refusal carries the server's code, and anything else that goes wrong a code of its own", async () => {
  const { api } = serve(
    { status: 401, body: { code: "sign_in.failed", retryable: false } },
    "unreachable",
    { status: 502 },
    { status: 500, body: { reason: "made up" } },
    { status: 200, body: { account: "acc-1" } },
    { status: 202, body: { challenge: 7 } },
  );
  const finish = () => api.finishSignIn({ email: "ada@example.com", code: "1", challenge: "ch-1" });

  assert.deepEqual(await finish(), { ok: false, code: "sign_in.failed" });
  assert.deepEqual(await finish(), { ok: false, code: UNREACHABLE });
  assert.deepEqual(await finish(), { ok: false, code: MALFORMED });
  assert.deepEqual(await finish(), { ok: false, code: MALFORMED });
  assert.deepEqual(await finish(), { ok: false, code: MALFORMED });
  assert.deepEqual(await api.startSignIn("ada@example.com"), { ok: false, code: MALFORMED });
});

test("workspaces are listed and made, and an answer of the wrong shape is refused", async () => {
  const personal = { id: "ws-1", name: "Personal", role: "owner" };
  const { api, sent } = serve(
    { status: 200, body: { workspaces: [personal] } },
    { status: 201, body: { id: "ws-2", name: "Made up", role: "owner" } },
    { status: 422, body: { code: "workspace.name_invalid", retryable: false } },
    { status: 200, body: { workspaces: [{ id: "ws-1", name: "Personal" }] } },
    { status: 200, body: { workspaces: "none" } },
    { status: 200, body: { workspaces: [{ ...personal, id: "a/b" }] } },
  );

  assert.deepEqual(await api.workspaces(), { ok: true, value: { workspaces: [personal] } });
  assert.deepEqual(await api.createWorkspace("Made up"), { ok: true, value: { id: "ws-2", name: "Made up", role: "owner" } });
  assert.deepEqual(await api.createWorkspace(" "), { ok: false, code: "workspace.name_invalid" });
  assert.deepEqual(await api.workspaces(), { ok: false, code: MALFORMED });
  assert.deepEqual(await api.workspaces(), { ok: false, code: MALFORMED });
  assert.deepEqual(await api.workspaces(), { ok: false, code: MALFORMED });
  assert.deepEqual(sent.slice(0, 2), [
    { path: "/api/workspaces", method: "GET" },
    { path: "/api/workspaces", method: "POST", headers: { "content-type": "application/json" }, body: '{"name":"Made up"}' },
  ]);
});

test("whenever the server says the session is over, the page is told", async () => {
  const over = { status: 401, body: { code: "sign_in.required", retryable: false } };
  const { api, ended } = serve(over, { status: 401, body: { code: "sign_in.failed", retryable: false } }, over);

  assert.deepEqual(await api.workspaces(), { ok: false, code: "sign_in.required" });
  assert.equal(ended(), 1);
  await api.finishSignIn({ email: "ada@example.com", code: "1", challenge: "ch-1" });
  assert.equal(ended(), 1);
  await api.createWorkspace("Made up");
  assert.equal(ended(), 2);
});

test("a request sent before a newer sign-in can't end it, but one before a failed sign-in can", async () => {
  type Response = Awaited<ReturnType<Fetch>>;
  const answer = (status: number, body: unknown): Response => ({ status, json: () => Promise.resolve(body) });
  const over = answer(401, { code: "sign_in.required", retryable: false });
  for (const [signIn, ended] of [
    [answer(200, ADA), 0],
    [answer(401, { code: "sign_in.failed", retryable: false }), 1],
  ] as const) {
    let ends = 0;
    let answerLate = (_: Response) => {};
    const answers: Array<() => Promise<Response>> = [
      () => new Promise((resolve) => (answerLate = resolve)),
      () => Promise.resolve(signIn),
    ];
    const api = new Api(() => answers.shift()?.() ?? Promise.reject(new TypeError("none")), () => ends++);

    const late = api.workspaces();
    await api.finishSignIn({ email: "ada@example.com", code: "12345678", challenge: "ch-1" });
    answerLate(over);

    assert.deepEqual(await late, { ok: false, code: "sign_in.required" });
    assert.equal(ends, ended);
  }
});

const KEY = "ab".repeat(32);
const MACHINE = { id: "mc-1", name: "Studio", os: "linux", key: KEY, paired_at: 1_763_000_000 };
const PENDING = { pairing: "p-1", name: "Studio", os: "macos", key: KEY };

test("machines are listed, looked up by code, confirmed and unpaired", async () => {
  const { api, sent } = serve(
    { status: 200, body: { machines: [MACHINE] } },
    { status: 200, body: PENDING },
    { status: 201, body: MACHINE },
    { status: 204 },
  );

  assert.deepEqual(await api.machines(), { ok: true, value: { machines: [MACHINE] } });
  assert.deepEqual(await api.lookUpPairing(" abcd-1234 "), { ok: true, value: PENDING });
  assert.deepEqual(await api.confirmPairing({ pairing: "p-1", code: "abcd-1234" }), { ok: true, value: MACHINE });
  assert.deepEqual(await api.unpair("mc-1"), { ok: true, value: undefined });
  const json = { "content-type": "application/json" };
  assert.deepEqual(sent, [
    { path: "/api/machines", method: "GET" },
    { path: "/api/machines/pair/lookup", method: "POST", headers: json, body: '{"code":" abcd-1234 "}' },
    { path: "/api/machines/pair/confirm", method: "POST", headers: json, body: '{"pairing":"p-1","code":"abcd-1234"}' },
    { path: "/api/machines/mc-1", method: "DELETE" },
  ]);
});

test("a machine whose id, key or date isn't one is a malformed answer, and a newer system is kept", async () => {
  const bad = [{ ...MACHINE, id: "../x" }, { ...MACHINE, key: "AB".repeat(32) }, { ...MACHINE, paired_at: "today" }];
  const { api } = serve(
    ...bad.map((machine) => ({ status: 200, body: { machines: [machine] } })),
    { status: 200, body: { ...PENDING, key: "short" } },
    { status: 200, body: { machines: [{ ...MACHINE, os: "plan9" }] } },
    { status: 404, body: { code: "pairing.not_found", retryable: false } },
  );

  for (const _ of bad) {
    assert.deepEqual(await api.machines(), { ok: false, code: MALFORMED });
  }
  assert.deepEqual(await api.lookUpPairing("abcd"), { ok: false, code: MALFORMED });
  assert.deepEqual(await api.machines(), { ok: true, value: { machines: [{ ...MACHINE, os: "plan9" }] } });
  assert.deepEqual(await api.confirmPairing({ pairing: "p-1", code: "abcd" }), { ok: false, code: "pairing.not_found" });
});

test("a workspace's chats are listed, and an answer of the wrong shape is refused", async () => {
  const chat = { id: "c-1", title: "Merge the lists", owner: "m-1", machine: "laptop", state: "open", updated_at: 1_800_000_000 };
  const { api, sent } = serve(
    { status: 200, body: { chats: [chat] } },
    { status: 200, body: { chats: [{ ...chat, id: "a/b" }] } },
    { status: 200, body: { chats: [{ ...chat, updated_at: "now" }] } },
    { status: 200, body: { chats: [{ ...chat, machine: undefined }] } },
    { status: 200, body: { chats: "none" } },
    { status: 403, body: { code: "workspace.not_member", retryable: false } },
  );

  assert.deepEqual(await api.chats("ws-1"), { ok: true, value: { chats: [chat] } });
  for (let left = 4; left > 0; left -= 1) {
    assert.deepEqual(await api.chats("ws-1"), { ok: false, code: MALFORMED });
  }
  assert.deepEqual(await api.chats("ws-1"), { ok: false, code: "workspace.not_member" });
  assert.deepEqual(sent[0], { path: "/api/workspaces/ws-1/chats", method: "GET" });
});

test("a machine already unpaired elsewhere counts as unpaired, and other failures stay failures", async () => {
  const { api } = serve({ status: 404, body: { code: "machine.not_found", retryable: false } }, { status: 409, body: { code: "machine.busy", retryable: false } }, "unreachable");

  assert.deepEqual(await api.unpair("mc-1"), { ok: true, value: undefined });
  assert.deepEqual(await api.unpair("mc-1"), { ok: false, code: "machine.busy" });
  assert.deepEqual(await api.unpair("mc-1"), { ok: false, code: UNREACHABLE });
});
