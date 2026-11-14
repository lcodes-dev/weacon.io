// The apps bundle `PROTOCOL`, so this file imports nothing at run time.

/** Must equal `weacon_protocol::PROTOCOL`. */
export const PROTOCOL = 1;

/** Whether `text` is a commit hash, by the rule `crates/version` applies too. */
function isCommit(text: string): boolean {
  return /^[0-9a-f]{7,64}$/i.test(text);
}

/** The workspace's version, from the root `Cargo.toml`, so every program reports the same one. */
export function workspaceVersion(cargoToml: string): string {
  const section = cargoToml.split(/^\[/m).find((part) => part.startsWith("workspace.package]"));
  const version = section?.match(/^version = "([^"]+)"$/m)?.[1];
  if (version === undefined) {
    throw new Error("Cargo.toml has no [workspace.package] version");
  }
  return version;
}

/** `env` without the variables that would point git at another repository than the one it runs in. */
export function gitEnv(env: NodeJS.ProcessEnv): NodeJS.ProcessEnv {
  const { GIT_DIR: _dir, GIT_WORK_TREE: _tree, GIT_INDEX_FILE: _index, ...rest } = env;
  return rest;
}

/** The commit being built: `given` when not blank, else git's; `unknown` unless it's a hash. */
export function commit(given: string | undefined, git: () => string | undefined): string {
  const found = given?.trim() || git()?.trim() || "";
  return isCommit(found) ? found : "unknown";
}
