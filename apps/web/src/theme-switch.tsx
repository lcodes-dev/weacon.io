import { useState } from "react";

import { STORE } from "./shell/last-workspace.ts";
import { type Theme, keepTheme, keptTheme, showTheme } from "./theme.ts";

/** A button that switches the page between dark and light, and keeps the choice in this browser. */
export function ThemeSwitch() {
  const [theme, setTheme] = useState(() => keptTheme(STORE));
  const next: Theme = theme === "dark" ? "light" : "dark";
  const label = `Switch to ${next}`;
  return (
    <button
      type="button"
      className="icon"
      aria-label={label}
      title={label}
      onClick={() => {
        keepTheme(STORE, next);
        showTheme(document.documentElement, next);
        setTheme(next);
      }}
    >
      <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
        {next === "light" ? (
          <g fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round">
            <circle cx="8" cy="8" r="3" />
            <path d="M8 1.5v1.5M8 13v1.5M1.5 8H3M13 8h1.5M3.4 3.4l1 1M11.6 11.6l1 1M3.4 12.6l1-1M11.6 4.4l1-1" />
          </g>
        ) : (
          <path d="M13.5 9.5A5.5 5.5 0 0 1 6.5 2.5a5.5 5.5 0 1 0 7 7Z" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinejoin="round" />
        )}
      </svg>
    </button>
  );
}
