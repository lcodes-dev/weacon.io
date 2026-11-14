import type { ReactNode } from "react";

/** The page under the Weacon header. */
export function Frame({ middle, right, children }: { middle?: ReactNode; right?: ReactNode; children: ReactNode }) {
  return (
    <div className="app">
      <header>
        <span>Weacon</span>
        {middle}
        {right}
      </header>
      <main>{children}</main>
    </div>
  );
}
