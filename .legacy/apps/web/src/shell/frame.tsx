import { type ReactNode, createContext, useState } from "react";

import { ThemeSwitch } from "../theme-switch.tsx";

/** Where an open workspace puts its sidebar, between the mark and the person's card. */
export const SideSlot = createContext<HTMLElement | null>(null);

/**
 * The page beside the sidebar, with the mark at its top and `card`, who is signed in, at its foot;
 * without a card, a bar with the mark and the theme switch.
 */
export function Frame({ card, children }: { card?: ReactNode; children: ReactNode }) {
  const [slot, setSlot] = useState<HTMLElement | null>(null);
  return (
    <div className={card === undefined ? "app" : "app railed"}>
      <aside className="rail">
        <span className="brand">
          <img src="icon.svg" alt="" width="28" height="28" />
          weacon
        </span>
        {card !== undefined && <div className="slot" ref={setSlot} />}
        {card ?? <ThemeSwitch />}
      </aside>
      <main>
        <SideSlot value={slot}>{children}</SideSlot>
      </main>
    </div>
  );
}
