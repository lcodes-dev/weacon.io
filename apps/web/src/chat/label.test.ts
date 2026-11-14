import assert from "node:assert/strict";
import { test } from "node:test";

import { type Names, type RunAgent, label } from "./label.ts";

const names: Names = {
  members: new Map([["m-1", "Ada"]]),
  machines: new Map([
    ["laptop-1", "laptop"],
    ["vps-1", "team VPS"],
  ]),
};

test("a label names the agent, whose it is and where it runs, as far as that is known", () => {
  const cases: [RunAgent | undefined, string][] = [
    [{ kind: "claude", owner: { kind: "member", member: "m-1" }, machine: "laptop-1" }, "Ada's Claude · laptop"],
    [{ kind: "codex", owner: { kind: "shared" }, machine: "vps-1" }, "Shared Codex · team VPS"],
    [{ kind: "claude", owner: { kind: "member", member: "m-1" } }, "Ada's Claude"],
    [{ kind: "claude", owner: { kind: "member", member: "m-1" }, machine: "gone" }, "Ada's Claude"],
    [{ kind: "claude", owner: { kind: "member", member: "m-9" }, machine: "laptop-1" }, "Claude · laptop"],
    [{ kind: "codex", owner: { kind: "shared" } }, "Shared Codex"],
    [{ kind: "claude" }, "Claude"],
    [{ kind: "newer", owner: { kind: "member", member: "m-1" } }, "Ada's Agent"],
    [{}, "Agent"],
    [undefined, "Agent"],
  ];

  for (const [agent, expected] of cases) {
    assert.equal(label(agent, names), expected, JSON.stringify(agent));
  }
});
