import assert from "node:assert/strict";
import { test } from "node:test";

import { buildVersion } from "./build.ts";

const ROOT = new URL("../../", import.meta.url);

test("a build takes the workspace's version and the given commit, else git's", () => {
  const given = buildVersion(ROOT, { ...process.env, WEACON_COMMIT: "1a2b3c4" });
  const found = buildVersion(ROOT, { ...process.env, WEACON_COMMIT: "" });

  assert.match(given.version, /^\d+\.\d+\.\d+$/);
  assert.equal(given.commit, "1a2b3c4");
  assert.match(found.commit, /^([0-9a-f]{40}|unknown)$/);
});
