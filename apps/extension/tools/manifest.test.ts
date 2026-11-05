import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import { commit, release, workspaceVersion } from "./manifest.ts";

const read = (path: string) => readFileSync(new URL(path, import.meta.url), "utf8");

test("the version is the workspace's", () => {
  const toml = '[package]\nversion = "9.9.9"\n\n[workspace.package]\nedition = "2024"\nversion = "0.1.0"\n';

  assert.equal(workspaceVersion(toml), "0.1.0");
  assert.match(workspaceVersion(read("../../../Cargo.toml")), /^\d+\.\d+\.\d+$/);
  assert.throws(() => workspaceVersion('[package]\nversion = "1.0.0"\n'), /no \[workspace.package\] version/);
});

test("the commit comes from the environment, then git, and must be a hash", () => {
  const sha = "1a2b3c4d5e6f";
  const none = () => undefined;

  assert.equal(commit(` ${sha} `, () => "ffffff0"), sha);
  assert.equal(commit("  ", () => `${sha}\n`), sha);
  assert.equal(commit(undefined, none), "unknown");
  assert.equal(commit("not-a-hash", none), "unknown");
  assert.equal(commit("1a2b3c", none), "unknown");
});

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
