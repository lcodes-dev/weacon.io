import { isCommit } from "../src/commit.ts";

/** The workspace's version, from the root `Cargo.toml`, so every program reports the same one. */
export function workspaceVersion(cargoToml: string): string {
  const section = cargoToml.split(/^\[/m).find((part) => part.startsWith("workspace.package]"));
  const version = section?.match(/^version = "([^"]+)"$/m)?.[1];
  if (version === undefined) {
    throw new Error("Cargo.toml has no [workspace.package] version");
  }
  return version;
}

/** The commit being built: `given` when not blank, else git's; `unknown` unless it's a hash. */
export function commit(given: string | undefined, git: () => string | undefined): string {
  const found = given?.trim() || git()?.trim() || "";
  return isCommit(found) ? found : "unknown";
}

/** The manifest Chrome loads: `source` with the build's version, and its commit in `version_name`. */
export function release(source: Record<string, unknown>, version: string, built: string): Record<string, unknown> {
  return { ...source, version, version_name: `${version} (${built})` };
}
