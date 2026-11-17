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
  return { api: new Api(fetch, () => ends++, () => {}), sent, ended: () => ends };
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
    { status: 200, body: { workspaces: [personal], invitations: [] } },
    { status: 201, body: { id: "ws-2", name: "Made up", role: "owner" } },
    { status: 422, body: { code: "workspace.name_invalid", retryable: false } },
    { status: 200, body: { workspaces: [{ id: "ws-1", name: "Personal" }] } },
    { status: 200, body: { workspaces: "none" } },
    { status: 200, body: { workspaces: [{ ...personal, id: "a/b" }] } },
    { status: 200, body: { workspaces: [{ ...personal, role: "auditor" }] } },
  );

  assert.deepEqual(await api.workspaces(), { ok: true, value: { workspaces: [personal], invitations: [] } });
  assert.deepEqual(await api.createWorkspace("Made up"), { ok: true, value: { id: "ws-2", name: "Made up", role: "owner" } });
  assert.deepEqual(await api.createWorkspace(" "), { ok: false, code: "workspace.name_invalid" });
  for (const _ of ["no role", "no list", "an odd id", "an odd role"]) {
    assert.deepEqual(await api.workspaces(), { ok: false, code: MALFORMED });
  }
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

test("a workspace refused to a session that still is one is left, and one refused after the session ended signs out", async () => {
  const over = { status: 401, body: { code: "sign_in.required", retryable: false } };
  const sent: string[] = [];
  const answers = [over, { status: 200, body: ADA }, over, over];
  const fetch: Fetch = (path) => {
    sent.push(path);
    const answer = answers.shift() ?? over;
    return Promise.resolve({ status: answer.status, json: () => Promise.resolve(answer.body) });
  };
  let ends = 0;
  const left: string[] = [];
  const api = new Api(fetch, () => ends++, (workspace) => left.push(workspace));

  assert.deepEqual(await api.members("w 1"), { ok: false, code: "sign_in.required" });
  assert.deepEqual([left, ends], [["w 1"], 0]);
  assert.deepEqual(await api.workspace("w-2"), { ok: false, code: "sign_in.required" });
  assert.deepEqual([left, ends], [["w 1"], 1]);
  assert.deepEqual(sent, ["/api/workspaces/w%201/members", "/api/session", "/api/workspaces/w-2", "/api/session"]);
});

test("members are changed, invitations taken up, and lists with invitations of the wrong shape refused", async () => {
  const offer = { workspace: "w-1", name: "Crew", expires_at: 9 };
  const invitation = { email: "bo@example.com", expires_at: 9 };
  const { api, sent } = serve(
    { status: 204 },
    { status: 200, body: { id: "w-1", name: "Crew", role: "member" } },
    { status: 200, body: { workspaces: [], invitations: [offer] } },
    { status: 200, body: { workspaces: [], invitations: [{ ...offer, workspace: "a/b" }] } },
    { status: 200, body: { workspaces: [], invitations: [{ ...offer, expires_at: "soon" }] } },
    { status: 200, body: { members: [{ id: "m-1", name: "ada", role: "owner" }], invitations: [invitation] } },
    { status: 200, body: { members: [], invitations: [{ ...invitation, expires_at: undefined }] } },
    { status: 200, body: { members: [{ id: "m-1", name: "ada", role: "auditor" }] } },
  );

  assert.deepEqual(await api.changeMembers("w-1", { kind: "invite", email: "bo@example.com" }), { ok: true, value: undefined });
  assert.deepEqual(await api.acceptInvitation("w-1"), { ok: true, value: { id: "w-1", name: "Crew", role: "member" } });
  assert.deepEqual(await api.workspaces(), { ok: true, value: { workspaces: [], invitations: [offer] } });
  assert.deepEqual(await api.workspaces(), { ok: false, code: MALFORMED });
  assert.deepEqual(await api.workspaces(), { ok: false, code: MALFORMED });
  assert.equal((await api.members("w-1")).ok, true);
  assert.deepEqual(await api.members("w-1"), { ok: false, code: MALFORMED });
  assert.deepEqual(await api.members("w-1"), { ok: false, code: MALFORMED });
  const json = { "content-type": "application/json" };
  assert.deepEqual(sent.slice(0, 2), [
    { path: "/api/workspaces/w-1/members", method: "POST", headers: json, body: '{"action":{"kind":"invite","email":"bo@example.com"}}' },
    { path: "/api/workspaces/w-1/accept", method: "POST" },
  ]);
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
    const api = new Api(() => answers.shift()?.() ?? Promise.reject(new TypeError("none")), () => ends++, () => {});

    const late = api.workspaces();
    await api.finishSignIn({ email: "ada@example.com", code: "12345678", challenge: "ch-1" });
    answerLate(over);

    assert.deepEqual(await late, { ok: false, code: "sign_in.required" });
    assert.equal(ends, ended);
  }
});

const MACHINE = { id: "mc-1", name: "Studio", os: "linux", paired_at: 1_763_000_000 };
const PENDING = { pairing: "p-1", name: "Studio", os: "macos" };

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

test("the machines shared with a workspace are listed to its members, and an answer of the wrong shape is refused", async () => {
  const shared = { id: "mc-9", name: "Team VPS", owner: "m-1", connected: true };
  const { api, sent } = serve({ status: 200, body: { machines: [shared] } }, { status: 200, body: { machines: [{ id: "mc-9", name: "Team VPS" }] } });

  assert.deepEqual(await api.sharedMachines("w-1"), { ok: true, value: { machines: [shared] } });
  assert.deepEqual(await api.sharedMachines("w-1"), { ok: false, code: MALFORMED });
  assert.deepEqual(sent[0], { path: "/api/workspaces/w-1/machines", method: "GET" });
});

test("a machine whose id, date or system isn't one is a malformed answer", async () => {
  const bad = [
    { ...MACHINE, id: "../x" },
    { ...MACHINE, paired_at: "today" },
    { ...MACHINE, offers: "worktree.open_terminal" },
    { ...MACHINE, offers: [7] },
    { ...MACHINE, connected: "yes" },
    { ...MACHINE, os: "plan9" },
  ];
  const { api } = serve(
    ...bad.map((machine) => ({ status: 200, body: { machines: [machine] } })),
    { status: 200, body: { ...PENDING, pairing: "../x" } },
    { status: 200, body: { ...PENDING, os: "plan9" } },
    { status: 200, body: { machines: [{ ...MACHINE, offers: ["worktree.open_terminal"], connected: true }] } },
    { status: 404, body: { code: "pairing.not_found", retryable: false } },
  );

  for (const _ of bad) {
    assert.deepEqual(await api.machines(), { ok: false, code: MALFORMED });
  }
  assert.deepEqual(await api.lookUpPairing("abcd"), { ok: false, code: MALFORMED });
  assert.deepEqual(await api.lookUpPairing("abcd"), { ok: false, code: MALFORMED });
  assert.deepEqual(await api.machines(), { ok: true, value: { machines: [{ ...MACHINE, offers: ["worktree.open_terminal"], connected: true }] } });
  assert.deepEqual(await api.confirmPairing({ pairing: "p-1", code: "abcd" }), { ok: false, code: "pairing.not_found" });
});

test("a workspace's chats are listed, and an answer of the wrong shape is refused", async () => {
  const leg = { member: "m-1", machine: "laptop", machine_id: "mc-1", agent: "claude", detached: false };
  const chat = { id: "c-1", title: "Merge the lists", owner: "m-1", legs: [leg], state: "open", updated_at: 1_800_000_000 };
  const { api, sent } = serve(
    { status: 200, body: { chats: [chat] } },
    { status: 200, body: { chats: [{ ...chat, run: "waiting" }] } },
    { status: 200, body: { chats: [{ ...chat, task: "t-1" }] } },
    { status: 200, body: { chats: [{ ...chat, legs: [] }] } },
    { status: 200, body: { chats: [{ ...chat, run: 7 }] } },
    { status: 200, body: { chats: [{ ...chat, run: "paused" }] } },
    { status: 200, body: { chats: [{ ...chat, state: "archived" }] } },
    { status: 200, body: { chats: [{ ...chat, legs: [{ ...leg, agent: "gemini" }] }] } },
    { status: 200, body: { chats: [{ ...chat, task: "../t" }] } },
    { status: 200, body: { chats: [{ ...chat, legs: [{ ...leg, machine_id: "../mc" }] }] } },
    { status: 200, body: { chats: [{ ...chat, legs: [{ ...leg, detached: undefined }] }] } },
    { status: 200, body: { chats: [{ ...chat, id: "a/b" }] } },
    { status: 200, body: { chats: [{ ...chat, updated_at: "now" }] } },
    { status: 200, body: { chats: [{ ...chat, legs: undefined }] } },
    { status: 200, body: { chats: "none" } },
    { status: 403, body: { code: "workspace.not_member", retryable: false } },
  );

  assert.deepEqual(await api.chats("ws-1"), { ok: true, value: { chats: [chat] } });
  assert.deepEqual(await api.chats("ws-1"), { ok: true, value: { chats: [{ ...chat, run: "waiting" }] } });
  assert.deepEqual(await api.chats("ws-1"), { ok: true, value: { chats: [{ ...chat, task: "t-1" }] } });
  // Starting a chat needs no agent.
  assert.deepEqual(await api.chats("ws-1"), { ok: true, value: { chats: [{ ...chat, legs: [] }] } });
  for (let left = 11; left > 0; left -= 1) {
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

test("a search posts its text and takes hits it knows, but nothing malformed", async () => {
  const snippet = { text: "the plan", marks: [{ start: 4, end: 8 }] };
  const hits = [
    { kind: "task", task: "t-1", title: "Plan", snippet },
    { kind: "chat", chat: "c-1", title: "Fix the build", seq: 3, snippet },
  ];
  const { api, sent } = serve(
    { status: 200, body: { hits } },
    { status: 200, body: { hits: [{ kind: "chat", chat: "c 1", title: "Bad id", snippet }] } },
    { status: 200, body: { hits: [{ kind: "doc", doc: "d-1", title: "Plan", snippet: { text: "x", marks: [{ start: "0" }] } }] } },
    { status: 200, body: { hits: [{ kind: "task", task: { toString: null }, title: "Plan", snippet }] } },
    { status: 200, body: { hits: [{ kind: "chat", chat: "c-1", title: "Plan", seq: { toString: null }, snippet }] } },
    { status: 200, body: { hits: [{ kind: "chat", chat: "c-1", title: "Plan", seq: -1, snippet }] } },
    { status: 200, body: { hits: [{ kind: "symbol", name: "a::b" }] } },
  );

  assert.deepEqual(await api.search("w-1", { text: "plan" }), { ok: true, value: { hits } });
  assert.deepEqual(await api.search("w-1", { text: "bad" }), { ok: false, code: MALFORMED });
  for (const text of ["worse", "task", "seq", "negative", "kind"]) {
    assert.deepEqual(await api.search("w-1", { text }), { ok: false, code: MALFORMED });
  }
  assert.deepEqual(sent[0], { path: "/api/workspaces/w-1/search", method: "POST", headers: { "content-type": "application/json" }, body: '{"text":"plan"}' });
});

test("a decision's outcome is taken only with the fields its kind needs", async () => {
  const conflict = { outcome: "doc_conflict", title: false, anchors: true, hunks: [{ kind: "agreed", text: "a\n" }, { kind: "conflict", base: "b", current: "c", proposed: "d" }] };
  const { api, sent } = serve(
    { status: 200, body: conflict },
    { status: 200, body: { outcome: "doc_conflict" } },
    { status: 200, body: { ...conflict, hunks: [{ kind: "conflict", base: "b" }] } },
    { status: 200, body: { ...conflict, hunks: [{ kind: "moved" }] } },
    { status: 200, body: { outcome: "accepted" } },
    { status: 200, body: { outcome: "task_conflict", edits: [{ kind: "move" }] } },
    { status: 200, body: { outcome: "task_conflict", edits: [7] } },
    { status: 200, body: { outcome: "queued" } },
    { status: 200, body: { outcome: "rejected" } },
    { status: 409, body: { code: "proposal.closed", retryable: false } },
  );

  assert.deepEqual(await api.decide("w-1", "p-1", "accept"), { ok: true, value: conflict });
  for (let at = 0; at < 4; at++) {
    assert.deepEqual(await api.decide("w-1", "p-1", "accept"), { ok: false, code: MALFORMED });
  }
  assert.equal((await api.decide("w-1", "p-1", "accept")).ok, true);
  assert.deepEqual(await api.decide("w-1", "p-1", "accept"), { ok: false, code: MALFORMED });
  assert.deepEqual(await api.decide("w-1", "p-1", "accept"), { ok: false, code: MALFORMED });
  assert.deepEqual(await api.decide("w-1", "p-1", "reject"), { ok: true, value: { outcome: "rejected" } });
  assert.deepEqual(await api.decide("w-1", "p-1", "reject"), { ok: false, code: "proposal.closed" });
  assert.deepEqual(sent.at(-1), { path: "/api/workspaces/w-1/proposals/p-1", method: "POST", headers: { "content-type": "application/json" }, body: '{"verdict":"reject"}' });
});
