import assert from "node:assert/strict";
import { test } from "node:test";

import type { MachineSummary } from "../protocol.ts";
import { opensTerminals } from "./core.ts";

const machine: MachineSummary = { id: "mc-1", name: "Studio", os: "linux", paired_at: 1 };

test("a machine opens terminals only when its core said so", () => {
  assert.equal(opensTerminals(machine), false);
  assert.equal(opensTerminals({ ...machine, offers: [] }), false);
  assert.equal(opensTerminals({ ...machine, offers: ["worktree.open_terminal"] }), true);
});
