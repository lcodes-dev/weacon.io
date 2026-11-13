import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import { currentTypes } from "../../../tooling/ts/generate-types.ts";
import type { Envelope } from "../src/protocol.ts";

// The type checker rejects these if the generated types stop matching what the app sends.
export const SAMPLES: Envelope[] = [
  { id: 1, type: "hello", protocol: 1, app: { name: "weacon-web", version: "0.1.0", commit: "1a2b3c4" } },
  { id: 0, type: "log", level: "warn", msg: "connection lost", code: "connection.lost" },
  { id: 0, type: "error", code: "web.crashed", retryable: false },
];

test("the committed types are current", () => {
  assert.equal(
    readFileSync(new URL("../src/protocol.ts", import.meta.url), "utf8"),
    currentTypes(),
    "src/protocol.ts is out of date: run npm run update-types in apps/web",
  );
});
