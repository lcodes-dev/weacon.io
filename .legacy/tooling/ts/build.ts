import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";

import { commit, gitEnv, workspaceVersion } from "./version.ts";

/** The version and commit an app in the repository at `root` is built from; `WEACON_COMMIT` in `env` wins over git's. */
export function buildVersion(root: URL, env: NodeJS.ProcessEnv): { version: string; commit: string } {
  const git = () => {
    try {
      return execFileSync("git", ["rev-parse", "HEAD"], { cwd: root, env: gitEnv(env), encoding: "utf8", stdio: "pipe" });
    } catch {
      return undefined;
    }
  };
  const version = workspaceVersion(readFileSync(new URL("Cargo.toml", root), "utf8"));
  return { version, commit: commit(env["WEACON_COMMIT"], git) };
}
