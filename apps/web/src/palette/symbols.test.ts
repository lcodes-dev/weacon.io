import assert from "node:assert/strict";
import { test } from "node:test";

import type { MachineSummary, Symbol as CodeSymbol } from "../protocol.ts";
import { fake } from "../machines/fake-socket.ts";
import { place, searcher, symbols } from "./symbols.ts";

const CORE = { name: "weacon-core", version: "0.1.0", commit: "1a2b3c4" };

// A machine, connected unless `offline`, whose core offers symbols unless `older`; `silent` when its core never said.
const machine = (id: string, name: string, how: { offline?: boolean; older?: boolean; silent?: boolean } = {}): MachineSummary => ({
  id,
  name,
  os: "linux",
  paired_at: 1,
  ...(how.silent === true ? {} : { core: CORE }),
  ...(how.older === true ? { missing: ["symbols.search"] } : {}),
  ...(how.offline === true ? {} : { connected: true }),
});

const RUN: CodeSymbol = { repo: "site", kind: "fn", name: "a::run", path: "src/lib.rs", line: 2 };

test("the open chat's machine is asked when it offers symbols, else the first machine that does", () => {
  const machines = [
    machine("mc-1", "Laptop", { older: true }),
    machine("mc-2", "Studio"),
    machine("mc-3", "Server"),
    machine("mc-4", "Old", { offline: true }),
    machine("mc-5", "Quiet", { silent: true }),
  ];

  assert.equal(searcher(machines, "mc-3"), "mc-3");
  assert.equal(searcher(machines, "mc-1"), "mc-2");
  assert.equal(searcher(machines, "mc-4"), "mc-2");
  assert.equal(searcher(machines, "mc-5"), "mc-2");
  assert.equal(searcher(machines, undefined), "mc-2");
  assert.equal(searcher([machines[0]!, machines[3]!, machines[4]!], "mc-1"), undefined);
  assert.equal(searcher([], undefined), undefined);
});

test("a search asks the machine for the open workspace and takes its own answer, leaving out kinds it doesn't know", async () => {
  const core = fake();
  const asking = symbols(core.socket, "mc-2", "run", core.later);
  const [query] = core.sent;
  assert.ok(query?.type === "symbols_query");
  assert.deepEqual({ ...query, request: "r" }, {
    id: 11,
    type: "symbols_query",
    request: "r",
    machine: "mc-2",
    workspace: "w-1",
    query: { kind: "search", words: "run" },
  });
  const answer = (request: string, found: CodeSymbol[]) =>
    core.deliver({ id: 11, type: "symbols_answer", request, machine: "mc-2", workspace: "w-1", answer: { kind: "found", symbols: found, more: false } });
  answer("r-other", []);
  core.deliver({ id: 11, type: "sessions_answer", request: query.request, machine: "mc-2", answer: { kind: "missing" } });
  answer(query.request, [RUN, { ...RUN, kind: "class" as CodeSymbol["kind"] }]);

  assert.deepEqual(await asking, { ok: true, value: [RUN] });
  assert.equal(core.listeners(), 0);
});

test("a newer answer is malformed, and an error answers by the envelope's id", async () => {
  const newer = fake();
  const asking = symbols(newer.socket, "mc-2", "run", newer.later);
  const [query] = newer.sent;
  assert.ok(query?.type === "symbols_query");
  newer.deliver({ id: 11, type: "symbols_answer", request: query.request, machine: "mc-2", workspace: "w-1", answer: { kind: "later" } as never });
  assert.deepEqual(await asking, { ok: false, code: "answer.malformed" });

  const refused = fake();
  const refusing = symbols(refused.socket, "mc-2", "run", refused.later);
  refused.deliver({ id: 11, type: "error", code: "symbols.not_owner", retryable: false });
  assert.deepEqual(await refusing, { ok: false, code: "symbols.not_owner" });
});

test("a symbol is copied as its file and line", () => {
  assert.equal(place(RUN), "src/lib.rs:2");
});
