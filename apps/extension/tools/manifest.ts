/** The files the build copies from `src` as they are: the panel's page and style, the mark, and the icons `source` names. */
export function statics(source: Record<string, unknown>): string[] {
  const icons = source["icons"];
  const named = typeof icons === "object" && icons !== null ? Object.values(icons).filter((file) => typeof file === "string") : [];
  return ["panel.html", "panel.css", "icon.svg", ...named];
}

/** The manifest Chrome loads: `source` with the build's version, and its commit in `version_name`. */
export function release(source: Record<string, unknown>, version: string, built: string): Record<string, unknown> {
  return { ...source, version, version_name: `${version} (${built})` };
}
