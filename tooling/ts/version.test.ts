import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import { PROTOCOL, commit, gitEnv, workspaceVersion } from "./version.ts";

test("the version is the workspace's", () => {
  const toml = '[package]\nversion = "9.9.9"\n\n[workspace.package]\nedition = "2024"\nversion = "0.1.0"\n';

  assert.equal(workspaceVersion(toml), "0.1.0");
  assert.match(workspaceVersion(readFileSync(new URL("../../Cargo.toml", import.meta.url), "utf8")), /^\d+\.\d+\.\d+$/);
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

test("git runs on this repository whatever the environment points it at", () => {
  const env = { GIT_DIR: "/other/.git", GIT_WORK_TREE: "/other", GIT_INDEX_FILE: "/other/index", PATH: "/bin" };

  assert.deepEqual(gitEnv(env), { PATH: "/bin" });
});

test("the protocol version matches the Rust one", () => {
  const rust = readFileSync(new URL("../../crates/protocol/src/message.rs", import.meta.url), "utf8");

  assert.equal(rust.match(/pub const PROTOCOL: u32 = (\d+);/)?.[1], String(PROTOCOL));
});
