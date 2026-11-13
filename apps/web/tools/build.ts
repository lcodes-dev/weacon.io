import { copyFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import { build } from "esbuild";

import { buildVersion } from "../../../tooling/ts/build.ts";

const root = new URL("..", import.meta.url);
const dist = new URL("dist/", root);
const { version, commit } = buildVersion(new URL("../../", root), process.env);

// Fixed names, so the server can embed them at fixed paths.
await build({
  entryPoints: { app: "src/main.tsx" },
  bundle: true,
  minify: true,
  // A classic script, so the page also opens from a file in tests.
  format: "iife",
  target: "es2022",
  absWorkingDir: fileURLToPath(root),
  outdir: fileURLToPath(dist),
  define: {
    WEACON_VERSION: JSON.stringify(version),
    WEACON_COMMIT: JSON.stringify(commit),
    "process.env.NODE_ENV": '"production"',
  },
  logLevel: "warning",
});
copyFileSync(new URL("src/index.html", root), new URL("index.html", dist));
