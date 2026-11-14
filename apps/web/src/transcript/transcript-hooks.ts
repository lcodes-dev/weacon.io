import { type RefObject, type UIEventHandler, useEffect, useLayoutEffect, useReducer, useRef } from "react";

import { atBottom, followsAfter, reachesBottom, scrollKey, selectingIn, touches } from "./follow.ts";
import type { Row } from "./transcript.ts";

/**
 * Keeps `scroller` at the bottom as `rows` grow or `refreshes` counts up, until the reader scrolls
 * up or selects in it, and again once they're back at the bottom. While `away`, under Settings,
 * selections don't count.
 */
export function useFollow(
  scroller: RefObject<HTMLDivElement | null>,
  content: RefObject<HTMLDivElement | null>,
  { rows, refreshes, away }: { rows: readonly Row[]; refreshes: number; away: boolean },
): UIEventHandler<HTMLDivElement> {
  const following = useRef(true);
  // The reader is scrolling: from their input until the scroll it starts ends, or while they hold
  // the scrollbar or a touch. Only their scrolls take the view off the bottom.
  const reading = useRef(false);
  const grabbed = useRef(false);
  // Where the view was when the reader's input came: their scroll has begun once it leaves there.
  const readFrom = useRef<number | undefined>(undefined);
  const awayNow = useRef(away);
  useLayoutEffect(() => {
    awayNow.current = away;
  }, [away]);
  const keepAtBottom = useRef(() => {
    const element = scroller.current;
    if (element === null || !following.current || reading.current || grabbed.current) {
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
    const moved = () => readFrom.current !== undefined && element.scrollTop !== readFrom.current;
    const settle = () => {
      reading.current = false;
      readFrom.current = undefined;
      keepAtBottom.current();
    };
    const read = () => {
      reading.current = true;
      readFrom.current = element.scrollTop;
      // Input that moves nothing, as a wheel up at the top, ends with no scroll to end it.
      requestAnimationFrame(() =>
        requestAnimationFrame(() => {
          if (!moved()) {
            settle();
          }
        }),
      );
    };
    // The bottom moves while rows arrive, so a scroll aimed past it follows them at once.
    const follow = () => {
      following.current = true;
      settle();
    };
    const wheel = (event: WheelEvent) => {
      if (event.ctrlKey) {
        return;
      }
      if (reachesBottom(event.deltaY, element)) {
        follow();
      } else {
        read();
      }
    };
    const key = (event: KeyboardEvent) => {
      const target = event.target;
      const here = target === document.body || (target instanceof Node && element.contains(target));
      const scrolls = awayNow.current || !here ? undefined : scrollKey(event);
      if (scrolls === "end") {
        follow();
      } else if (scrolls !== undefined) {
        read();
      }
    };
    const grab = (event: PointerEvent) => {
      grabbed.current = event.button === 0;
    };
    const release = (event: Event) => {
      if (grabbed.current) {
        grabbed.current = false;
        // A touch flicked away goes on scrolling after it lifts.
        read();
      }
      if (event.type !== "pointerup") {
        settle();
      }
    };
    const stop = new AbortController();
    const { signal } = stop;
    // Not passive, so it runs before the view moves and no row can snap it back first.
    element.addEventListener("wheel", wheel, { passive: false, signal });
    element.addEventListener("pointerdown", grab, { signal });
    // A scroll the view made before the reader's input may still end after it.
    element.addEventListener("scrollend", () => moved() && settle(), { signal });
    for (const end of ["pointerup", "pointercancel", "contextmenu"]) {
      document.addEventListener(end, release, { signal });
    }
    window.addEventListener("blur", release, { signal });
    document.addEventListener("keydown", key, { signal });
    return () => {
      watch.disconnect();
      stop.abort();
    };
  }, [empty]);

  return ({ currentTarget: element }) => {
    following.current = followsAfter(following.current, atBottom(element), reading.current || grabbed.current);
  };
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
