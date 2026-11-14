// Runs in the page's isolated world: highlights the element under the pointer until a click
// picks it or Escape cancels.

import type { Rect } from "./screenshot.ts";
import { cssSelector } from "./selector.ts";

/** What the picker found, for the service worker to screenshot and send. */
export type Picked = {
  selector: string;
  html: string;
  title: string;
  url: string;
  rect: Rect;
  ratio: number;
};

/** The global the service worker calls after injecting this script. */
export type Picker = { weaconPick?: () => Promise<Picked | null> };

/** `start`, but while its promise is pending, calling again gives that promise. */
export function oneAtATime<T>(start: () => Promise<T>): () => Promise<T> {
  let pending: Promise<T> | undefined;
  return () =>
    (pending ??= start().finally(() => {
      pending = undefined;
    }));
}

// The page mustn't act on the presses that pick an element.
const SWALLOWED = ["pointerdown", "mousedown", "pointerup", "mouseup"] as const;

function pick(): Promise<Picked | null> {
  return new Promise((resolve) => {
    const box = document.createElement("div");
    Object.assign(box.style, {
      position: "fixed",
      zIndex: "2147483647",
      pointerEvents: "none",
      outline: "2px solid #2563eb",
      background: "rgba(37, 99, 235, 0.15)",
    });
    let target: Element | null = null;
    const move = (event: MouseEvent) => {
      target = event.target instanceof Element ? event.target : null;
      place(box, target);
    };
    const click = (event: MouseEvent) => {
      event.preventDefault();
      event.stopPropagation();
      // A click with no move before it has no target yet.
      finish(event.target instanceof Element ? event.target : target);
    };
    const swallow = (event: Event) => {
      event.preventDefault();
      event.stopPropagation();
    };
    const key = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        finish(null);
      }
    };
    const finish = (element: Element | null) => {
      removeEventListener("mousemove", move, true);
      removeEventListener("click", click, true);
      removeEventListener("keydown", key, true);
      SWALLOWED.forEach((type) => removeEventListener(type, swallow, true));
      box.remove();
      // Two frames, so the highlight is gone from the screen before the screenshot.
      requestAnimationFrame(() => requestAnimationFrame(() => resolve(element && describe(element))));
    };
    addEventListener("mousemove", move, true);
    addEventListener("click", click, true);
    addEventListener("keydown", key, true);
    SWALLOWED.forEach((type) => addEventListener(type, swallow, true));
    document.documentElement.append(box);
  });
}

function place(box: HTMLElement, element: Element | null) {
  if (element === null) {
    box.style.display = "none";
    return;
  }
  const { left, top, width, height } = element.getBoundingClientRect();
  Object.assign(box.style, {
    display: "block",
    left: `${left}px`,
    top: `${top}px`,
    width: `${width}px`,
    height: `${height}px`,
  });
}

function describe(element: Element): Picked {
  const { x, y, width, height } = element.getBoundingClientRect();
  return {
    selector: cssSelector(element, (id) => document.querySelectorAll(`#${CSS.escape(id)}`).length === 1),
    html: element.outerHTML,
    title: document.title,
    url: location.href,
    rect: { x, y, width, height },
    ratio: devicePixelRatio,
  };
}

(globalThis as Picker).weaconPick ??= oneAtATime(pick);
