import assert from "node:assert/strict";
import { test } from "node:test";

import { osName } from "./os.ts";

test("each system has its own name", () => {
  assert.deepEqual((["macos", "linux", "windows"] as const).map(osName), ["macOS", "Linux", "Windows"]);
});
