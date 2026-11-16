import assert from "node:assert/strict";
import { test } from "node:test";

import { agentItem, offered } from "./mentionables.ts";

const claude = agentItem("mc-1", "claude");
const shared = agentItem("mc-2", "codex", "Team VPS");

test("the writer's own agent goes by its vendor's name, and a shared machine's as the workspace's there", () => {
  assert.deepEqual(claude, { id: "mc-1", label: "Claude Code", note: "Your agent" });
  assert.deepEqual(shared, { id: "mc-2", label: "Shared Codex · Team VPS", note: "Shared agent" });
  assert.deepEqual(agentItem("mc-1", "codex").label, "Codex");
});

test("each agent is offered when a word of its name starts with the query, in any case", () => {
  const labels = (text: string) => offered({ start: 0, text }, undefined, false, [claude, shared]).map(({ label }) => label);

  assert.deepEqual(labels(""), ["Claude Code", "Shared Codex · Team VPS"]);
  assert.deepEqual(labels("CO"), ["Claude Code", "Shared Codex · Team VPS"]);
  assert.deepEqual(labels("cl"), ["Claude Code"]);
  assert.deepEqual(labels("team"), ["Shared Codex · Team VPS"]);
  assert.deepEqual(labels("x"), []);
});

test("the picker offers nothing without a query or an agent, once dismissed, or while a message is being sent", () => {
  const at = { start: 3, text: "co" };
  assert.deepEqual(offered(at, undefined, false, [claude]), [claude]);
  assert.deepEqual(offered(undefined, undefined, false, [claude]), []);
  assert.deepEqual(offered(at, undefined, false, []), []);
  assert.deepEqual(offered(at, 3, false, [claude]), []);
  assert.deepEqual(offered(at, 0, false, [claude]), [claude]);
  assert.deepEqual(offered(at, undefined, true, [claude]), []);
});
