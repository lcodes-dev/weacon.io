import assert from "node:assert/strict";
import { test } from "node:test";

import type { AgentRef } from "../protocol.ts";
import { label } from "./label.ts";

const members = new Map([["m-1", "Ada"]]);

test("a label names the agent, whose it is and where it runs, as far as that is known", () => {
  const cases: [AgentRef | undefined, string][] = [
    [{ kind: "claude", owner: { kind: "member", member: "m-1" }, machine: "laptop" }, "Ada's Claude · laptop"],
    [{ kind: "codex", owner: { kind: "shared" }, machine: "team VPS" }, "Shared Codex · team VPS"],
    [{ kind: "claude", owner: { kind: "member", member: "m-1" } }, "Ada's Claude"],
    [{ kind: "claude", owner: { kind: "member", member: "m-9" }, machine: "laptop" }, "Claude · laptop"],
    [{ kind: "codex", owner: { kind: "shared" } }, "Shared Codex"],
    [{ kind: "claude", owner: { kind: "member", member: "m-9" } }, "Claude"],
    [{ kind: "gemini" as AgentRef["kind"], owner: { kind: "member", member: "m-1" } }, "Ada's Agent"],
    [undefined, "Agent"],
  ];

  for (const [agent, expected] of cases) {
    assert.equal(label(agent, members), expected, JSON.stringify(agent));
  }
});
