import assert from "node:assert/strict";
import { test } from "node:test";

import { agentItem, offered } from "./mentionables.ts";

const claude = agentItem("claude");

test("the writer's agent is offered under its vendor's name when a word of it starts with the query, in any case", () => {
  const labels = (text: string) => offered({ start: 0, text }, undefined, false, claude).map(({ label }) => label);

  assert.deepEqual(claude, { id: "claude", label: "Claude Code" });
  assert.deepEqual(labels(""), ["Claude Code"]);
  assert.deepEqual(labels("CO"), ["Claude Code"]);
  assert.deepEqual(labels("cl"), ["Claude Code"]);
  assert.deepEqual(labels("x"), []);
  assert.deepEqual(agentItem("codex").label, "Codex");
});

test("the picker offers nothing without a query or an agent, once dismissed, or while a message is being sent", () => {
  const at = { start: 3, text: "co" };
  assert.deepEqual(offered(at, undefined, false, claude), [claude]);
  assert.deepEqual(offered(undefined, undefined, false, claude), []);
  assert.deepEqual(offered(at, undefined, false, undefined), []);
  assert.deepEqual(offered(at, 3, false, claude), []);
  assert.deepEqual(offered(at, 0, false, claude), [claude]);
  assert.deepEqual(offered(at, undefined, true, claude), []);
});
