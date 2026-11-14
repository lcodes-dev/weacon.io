import assert from "node:assert/strict";
import { test } from "node:test";

import { osName } from "./os.ts";

test("each system has its own name, and one this build doesn't know has a general one", () => {
  assert.deepEqual(["macos", "linux", "windows", "plan9"].map(osName), ["macOS", "Linux", "Windows", "Another system"]);
});
