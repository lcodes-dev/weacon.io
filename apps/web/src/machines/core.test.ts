import assert from "node:assert/strict";
import { test } from "node:test";

import type { MachineSummary } from "../protocol.ts";
import { coreVersion, isOutdated } from "./core.ts";

const machine: MachineSummary = {
  id: "mc-1",
  name: "Studio",
  os: "linux",
  key: "k",
  paired_at: 1,
};

test("a machine whose core hasn't connected shows no version and isn't outdated", () => {
  assert.equal(coreVersion(machine), "");
  assert.equal(isOutdated(machine), false);
});

test("a core's version shows, and it is outdated only when it lacks something the server offers", () => {
  const connected = { ...machine, core: { name: "weacon-core", version: "1.2.3", commit: "abc" } };
  assert.equal(coreVersion(connected), ", weacon 1.2.3");
  assert.equal(isOutdated(connected), false);
  assert.equal(isOutdated({ ...connected, missing: ["machine.settings"] }), true);
});
