import assert from "node:assert/strict";
import { test } from "node:test";

import type { AgentRef } from "../protocol.ts";
import { label } from "./label.ts";

test("a label names the agent, whether it is shared, and where it runs, as far as that is known", () => {
  const cases: [AgentRef | undefined, string][] = [
    [{ kind: "claude", owner: { kind: "member", member: "m-1" }, machine: "laptop" }, "Claude Code · laptop"],
    [{ kind: "codex", owner: { kind: "shared" }, machine: "team VPS" }, "Shared Codex · team VPS"],
    [{ kind: "claude", owner: { kind: "member", member: "m-1" } }, "Claude Code"],
    [{ kind: "claude", owner: { kind: "member", member: "m-9" }, machine: "laptop" }, "Claude Code · laptop"],
    [{ kind: "codex", owner: { kind: "shared" } }, "Shared Codex"],
    [{ kind: "claude", owner: { kind: "member", member: "m-9" } }, "Claude Code"],
    [{ kind: "gemini" as AgentRef["kind"], owner: { kind: "member", member: "m-1" } }, "Agent"],
    [undefined, "Agent"],
  ];

  for (const [agent, expected] of cases) {
    assert.equal(label(agent), expected, JSON.stringify(agent));
  }
});
