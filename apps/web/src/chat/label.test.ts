import assert from "node:assert/strict";
import { test } from "node:test";

import type { AgentRef } from "../protocol.ts";
import { label } from "./label.ts";

test("a label names the agent, whether it is shared, and where it runs, as far as that is known", () => {
  const cases: [AgentRef | undefined, string][] = [
    [{ kind: "claude", owner: { kind: "member", member: "m-1" }, machine: "laptop" }, "Claude Code · laptop"],
    [{ kind: "codex", owner: { kind: "shared", member: "m-2", owner: "m-1" }, machine: "team VPS" }, "Shared Codex · team VPS"],
    [{ kind: "claude", owner: { kind: "member", member: "m-1" } }, "Claude Code"],
    [{ kind: "codex", owner: { kind: "shared", member: "m-2", owner: "m-1" } }, "Shared Codex"],
    [{ kind: "gemini" as AgentRef["kind"], owner: { kind: "member", member: "m-1" } }, "Agent"],
    [undefined, "Agent"],
  ];

  for (const [agent, expected] of cases) {
    assert.equal(label(agent), expected, JSON.stringify(agent));
  }
});

test("a member's agent is labelled with its owner's name, once it is known", () => {
  const names = new Map([
    ["m-1", "ada"],
    ["m-2", "bo"],
  ]);

  assert.equal(label({ kind: "claude", owner: { kind: "member", member: "m-1" }, machine: "laptop" }, names), "ada's Claude Code · laptop");
  assert.equal(label({ kind: "codex", owner: { kind: "member", member: "m-9" } }, names), "Codex");
  assert.equal(label({ kind: "codex", owner: { kind: "shared", member: "m-2", owner: "m-1" }, machine: "team VPS" }, names, "m-2"), "Shared Codex · team VPS");
  assert.equal(label(undefined, names, "m-2"), "bo's agent");
  assert.equal(label({ kind: "claude", owner: { kind: "member", member: "m-1" } }, names, "m-2"), "ada's Claude Code");
});
