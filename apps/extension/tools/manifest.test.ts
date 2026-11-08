import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import { release } from "./manifest.ts";

const read = (path: string) => readFileSync(new URL(path, import.meta.url), "utf8");

test("the released manifest names its version and commit", () => {
  assert.deepEqual(release({ name: "Weacon", version: "x" }, "0.1.0", "1a2b3c4"), {
    name: "Weacon",
    version: "0.1.0",
    version_name: "0.1.0 (1a2b3c4)",
  });
});

test("the manifest's key gives the id the core lets start its native host", () => {
  const manifest = JSON.parse(read("../manifest.json")) as { key: string };
  const core = read("../../core/src/native_host.rs").match(/ORIGIN: &str = "chrome-extension:\/\/([a-p]{32})\/"/)?.[1];
  // Chrome's id: the key's SHA-256, its first 32 hex digits written with the letters a to p.
  const hash = createHash("sha256").update(Buffer.from(manifest.key, "base64")).digest("hex").slice(0, 32);
  const id = Array.from(hash, (digit) => String.fromCharCode(97 + Number.parseInt(digit, 16))).join("");

  assert.equal(id, core);
});
