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

  assert.deepEqual(await api.start("ada@example.com"), { ok: true, value: { challenge: "ch-1" } });
  assert.deepEqual(await api.finish({ email: "ada@example.com", code: "12345678", challenge: "ch-1" }), { ok: true, value: ADA });
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
  const { api, sent } = serve({ status: 204 }, { status: 403, body: { code: "request.cross_site", retryable: false } });

  assert.deepEqual(await api.signOut(), { ok: true, value: undefined });
  assert.deepEqual(await api.signOut(), { ok: false, code: "request.cross_site" });
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
  const finish = () => api.finish({ email: "ada@example.com", code: "1", challenge: "ch-1" });

  assert.deepEqual(await finish(), { ok: false, code: "sign_in.failed" });
  assert.deepEqual(await finish(), { ok: false, code: UNREACHABLE });
  assert.deepEqual(await finish(), { ok: false, code: MALFORMED });
  assert.deepEqual(await finish(), { ok: false, code: MALFORMED });
  assert.deepEqual(await finish(), { ok: false, code: MALFORMED });
  assert.deepEqual(await api.start("ada@example.com"), { ok: false, code: MALFORMED });
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
  assert.deepEqual(await api.create("Made up"), { ok: true, value: { id: "ws-2", name: "Made up", role: "owner" } });
  assert.deepEqual(await api.create(" "), { ok: false, code: "workspace.name_invalid" });
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
  await api.finish({ email: "ada@example.com", code: "1", challenge: "ch-1" });
  assert.equal(ended(), 1);
  await api.create("Made up");
  assert.equal(ended(), 2);
});
