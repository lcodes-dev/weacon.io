import type { Storing } from "./shell/last-workspace.ts";

/** How the app looks: dark unless the person chose light in this browser. */
export type Theme = "dark" | "light";

const KEY = "weacon.theme";

/** The theme this browser keeps, else dark. */
export function keptTheme(store: Storing): Theme {
  try {
    return store().getItem(KEY) === "light" ? "light" : "dark";
  } catch {
    return "dark";
  }
}

/** Keeps `theme` for the next visit. */
export function keepTheme(store: Storing, theme: Theme): void {
  try {
    store().setItem(KEY, theme);
  } catch {
    // Storage that is off or full only means the next visit starts dark.
  }
}

/** Shows the page in `theme`. */
export function showTheme(root: HTMLElement, theme: Theme): void {
  root.dataset["theme"] = theme;
}
