import type { ReactNode } from "react";

import { ThemeSwitch } from "../theme-switch.tsx";

/** The page under the Weacon header. */
export function Frame({ middle, right, children }: { middle?: ReactNode; right?: ReactNode; children: ReactNode }) {
  return (
    <div className="app">
      <header>
        <span className="brand">
          <img src="icon.svg" alt="" width="28" height="28" />
          weacon
        </span>
        {middle}
        <span className="end">
          {right}
          <ThemeSwitch />
        </span>
      </header>
      <main>{children}</main>
    </div>
  );
}
