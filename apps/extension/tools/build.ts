import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { build } from "esbuild";

import { commit, gitEnv, release, workspaceVersion } from "./manifest.ts";

const root = new URL("..", import.meta.url);
const dist = new URL("dist/", root);
const source = JSON.parse(readFileSync(new URL("manifest.json", root), "utf8")) as Record<string, unknown>;
const options = {
  bundle: true,
  target: `chrome${String(source["minimum_chrome_version"])}`,
  absWorkingDir: fileURLToPath(root),
  outdir: fileURLToPath(dist),
  logLevel: "warning",
} as const;

await build({ ...options, entryPoints: ["src/background.ts"], format: "esm" });
// Injected scripts can't be modules.
await build({ ...options, entryPoints: ["src/picker.ts"], format: "iife" });

const git = () => {
  try {
    const env = gitEnv(process.env);
    return execFileSync("git", ["rev-parse", "HEAD"], { cwd: root, env, encoding: "utf8", stdio: "pipe" });
  } catch {
    return undefined;
  }
};
const version = workspaceVersion(readFileSync(new URL("../../Cargo.toml", root), "utf8"));
const manifest = release(source, version, commit(process.env["WEACON_COMMIT"], git));
writeFileSync(new URL("manifest.json", dist), `${JSON.stringify(manifest, null, 2)}\n`);
