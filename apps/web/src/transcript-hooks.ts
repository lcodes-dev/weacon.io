import { type RefObject, type UIEventHandler, useEffect, useLayoutEffect, useReducer, useRef } from "react";

import { atBottom, followsAfter, leavesBottom, selectingIn, touches } from "./follow.ts";
import type { Row } from "./transcript.ts";

/**
 * Keeps `scroller` at the bottom as `rows` grow or `refreshes` counts up, until the reader scrolls
 * up or selects in it, and again once they are back at the bottom. Only the reader's own input
 * stops it: the browser also scrolls the view, as rows reach their height or the pane resizes.
 * While `away`, under Settings, selections don't count.
 */
export function useFollow(
  scroller: RefObject<HTMLDivElement | null>,
  content: RefObject<HTMLDivElement | null>,
  { rows, refreshes, away }: { rows: readonly Row[]; refreshes: number; away: boolean },
): UIEventHandler<HTMLDivElement> {
  const following = useRef(true);
  // The reader holds the scrollbar or a touch, so the view's scrolls are theirs.
  const held = useRef(false);
  const awayNow = useRef(away);
  useLayoutEffect(() => {
    awayNow.current = away;
  }, [away]);
  const keepAtBottom = useRef(() => {
    const element = scroller.current;
    if (element === null || !following.current || held.current) {
      return;
    }
    if (selectingIn(awayNow.current ? null : document.getSelection(), element)) {
      // Once the selection goes, follow only if the reader is still at the bottom.
      following.current = atBottom(element);
    } else {
      element.scrollTop = element.scrollHeight;
    }
  });
  useLayoutEffect(() => keepAtBottom.current(), [rows, refreshes]);

  const empty = rows.length === 0;
  useEffect(() => {
    const [element, inner] = [scroller.current, content.current];
    if (empty || element === null || inner === null) {
      return;
    }
    // Rows reach their real height only once shown, and the message box takes its room back when
    // Settings closes; a following reader stays at the bottom through both.
    const watch = new ResizeObserver(() => keepAtBottom.current());
    watch.observe(element);
    watch.observe(inner);
    const leave = () => {
      following.current = false;
    };
    const wheel = (event: WheelEvent) => {
      if (event.deltaY < 0) {
        leave();
      }
    };
    const key = (event: KeyboardEvent) => {
      if (!awayNow.current && leavesBottom(event, editable(event.target))) {
        leave();
      }
    };
    const hold = () => {
      held.current = true;
    };
    const release = () => {
      held.current = false;
    };
    element.addEventListener("wheel", wheel, { passive: true });
    element.addEventListener("pointerdown", hold);
    element.addEventListener("touchstart", hold, { passive: true });
    document.addEventListener("pointerup", release);
    document.addEventListener("touchend", release);
    document.addEventListener("keydown", key);
    return () => {
      watch.disconnect();
      element.removeEventListener("wheel", wheel);
      element.removeEventListener("pointerdown", hold);
      element.removeEventListener("touchstart", hold);
      document.removeEventListener("pointerup", release);
      document.removeEventListener("touchend", release);
      document.removeEventListener("keydown", key);
    };
  }, [empty]);

  return ({ currentTarget: element }) => {
    following.current = followsAfter(following.current, atBottom(element), held.current);
  };
}

// Whether keys pressed at `target` edit text there rather than scroll the page.
function editable(target: EventTarget | null): boolean {
  return target instanceof HTMLElement && (target.isContentEditable || ["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName));
}

/**
 * Which text rows of `scroller` may show as Markdown on this render, and a count that goes up
 * when one held back may now. While `away`, under Settings, selections don't count.
 */
export function useHeldFormatting(
  scroller: RefObject<HTMLDivElement | null>,
  away: boolean,
): { mayFormat: (at: number) => boolean; refreshes: number } {
  const formatted = useRef(new Set<number>());
  const held = useRef(false);
  const pressed = useRef(false);
  const [refreshes, refresh] = useReducer((count: number) => count + 1, 0);

  useEffect(() => {
    const update = () => {
      if (held.current) {
        refresh();
      }
    };
    const press = () => {
      pressed.current = true;
    };
    const release = () => {
      pressed.current = false;
      update();
    };
    document.addEventListener("selectionchange", update);
    document.addEventListener("pointerdown", press);
    document.addEventListener("pointerup", release);
    return () => {
      document.removeEventListener("selectionchange", update);
      document.removeEventListener("pointerdown", press);
      document.removeEventListener("pointerup", release);
    };
  }, []);

  const selection = away ? null : document.getSelection();
  held.current = false;
  // Once formatted, a row stays so, as reformatting would undo a selection in it. A row the
  // selection touches waits until it leaves; a bare caret holds it only while a button is down.
  const mayFormat = (at: number): boolean => {
    if (formatted.current.has(at)) {
      return true;
    }
    const element = scroller.current?.querySelector(`[data-index="${at}"]`);
    if (element && (selectingIn(selection, element) || (pressed.current && touches(selection, element)))) {
      held.current = true;
      return false;
    }
    formatted.current.add(at);
    return true;
  };
  return { mayFormat, refreshes };
}
