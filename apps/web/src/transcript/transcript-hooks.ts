import { type RefObject, type UIEventHandler, useEffect, useLayoutEffect, useReducer, useRef } from "react";

import { atBottom, selectingIn, touches } from "./follow.ts";

/**
 * Keeps `scroller` at the bottom as `rows` grow or `refreshes` counts up, unless the reader has
 * scrolled up or is selecting in it. While `away`, under Settings, selections don't count.
 */
export function useFollow(
  scroller: RefObject<HTMLDivElement | null>,
  content: RefObject<HTMLDivElement | null>,
  { rows, refreshes, away }: { rows: readonly unknown[]; refreshes: number; away: boolean },
): UIEventHandler<HTMLDivElement> {
  const following = useRef(true);
  // Where the view last scrolled itself. Its scroll event comes a frame later, maybe after the
  // pane has shrunk, as when the message box comes back, so it says nothing about the reader.
  const followedTo = useRef<number | undefined>(undefined);
  const awayNow = useRef(away);
  useLayoutEffect(() => {
    awayNow.current = away;
  }, [away]);
  const keepAtBottom = useRef(() => {
    const element = scroller.current;
    if (element === null || !following.current) {
      return;
    }
    if (selectingIn(awayNow.current ? null : document.getSelection(), element)) {
      // Once the selection goes, follow only if the reader is still at the bottom.
      following.current = atBottom(element);
    } else {
      element.scrollTop = element.scrollHeight;
      followedTo.current = element.scrollTop;
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
    return () => watch.disconnect();
  }, [empty]);

  return ({ currentTarget: element }) => {
    if (element.scrollTop !== followedTo.current) {
      followedTo.current = undefined;
      following.current = atBottom(element);
    }
  };
}

/**
 * Which text rows of `scroller` may show as Markdown on this render, and a count that goes up
 * when one held back may now. While `away`, under Settings, selections don't count.
 */
export function useHeldFormatting(
  scroller: RefObject<HTMLDivElement | null>,
  away: boolean,
): { mayFormat: (key: string) => boolean; refreshes: number } {
  const formatted = useRef(new Set<string>());
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
  const mayFormat = (key: string): boolean => {
    if (formatted.current.has(key)) {
      return true;
    }
    const element = scroller.current?.querySelector(`[data-key="${CSS.escape(key)}"]`);
    if (element && (selectingIn(selection, element) || (pressed.current && touches(selection, element)))) {
      held.current = true;
      return false;
    }
    formatted.current.add(key);
    return true;
  };
  return { mayFormat, refreshes };
}
