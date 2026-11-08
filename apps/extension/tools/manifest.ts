/** The manifest Chrome loads: `source` with the build's version, and its commit in `version_name`. */
export function release(source: Record<string, unknown>, version: string, built: string): Record<string, unknown> {
  return { ...source, version, version_name: `${version} (${built})` };
}
