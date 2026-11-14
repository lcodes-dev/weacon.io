import { readFileSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { build } from "esbuild";

import { buildVersion } from "../../../tooling/ts/build.ts";
import { release } from "./manifest.ts";

const root = new URL("..", import.meta.url);
const dist = new URL("dist/", root);
const source = JSON.parse(readFileSync(new URL("manifest.json", root), "utf8")) as Record<string, unknown>;
const { version, commit } = buildVersion(new URL("../../", root), process.env);
const options = {
  bundle: true,
  target: `chrome${String(source["minimum_chrome_version"])}`,
  absWorkingDir: fileURLToPath(root),
  outdir: fileURLToPath(dist),
  define: { WEACON_VERSION: JSON.stringify(version), WEACON_COMMIT: JSON.stringify(commit) },
  logLevel: "warning",
} as const;

await build({ ...options, entryPoints: ["src/background.ts"], format: "esm" });
// Injected scripts can't be modules.
await build({ ...options, entryPoints: ["src/picker.ts"], format: "iife" });

const manifest = release(source, version, commit);
writeFileSync(new URL("manifest.json", dist), `${JSON.stringify(manifest, null, 2)}\n`);
