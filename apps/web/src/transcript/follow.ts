// How close to the bottom still counts as there, for rounding and a last partial line.
const SLACK_PX = 32;

/** The scroll position of an element. */
export type Scroll = { scrollTop: number; scrollHeight: number; clientHeight: number };

export function atBottom(scroll: Scroll): boolean {
  return scroll.scrollHeight - scroll.scrollTop - scroll.clientHeight <= SLACK_PX;
}

/** Whether `selection` has text selected that reaches into `container`. */
export function selectingIn(
  selection: Pick<Selection, "isCollapsed" | "rangeCount" | "getRangeAt"> | null,
  container: Node,
): boolean {
  return selection !== null && !selection.isCollapsed && touches(selection, container);
}

/** Whether `selection`'s range touches `node`, even as a bare caret, as when a drag starts. */
export function touches(selection: Pick<Selection, "rangeCount" | "getRangeAt"> | null, node: Node): boolean {
  return selection !== null && selection.rangeCount > 0 && selection.getRangeAt(0).intersectsNode(node);
}

/** Whether a key press scrolls the page: to the end, some other way, or not at all. */
export function scrollKey({ key }: Pick<KeyboardEvent, "key">): "end" | "scroll" | undefined {
  if (key === "End") {
    return "end";
  }
  return ["ArrowUp", "ArrowDown", "PageUp", "PageDown", "Home", " "].includes(key) ? "scroll" : undefined;
}

/** Whether the view still follows after a scroll: only one the reader makes can take it off the bottom. */
export function followsAfter(following: boolean, bottom: boolean, readers: boolean): boolean {
  return bottom || (following && !readers);
}

/** Whether scrolling `delta` pixels down from `scroll` reaches the bottom, so the reader means to follow. */
export function reachesBottom(delta: number, scroll: Scroll): boolean {
  const { scrollTop, scrollHeight, clientHeight } = scroll;
  return delta > 0 && atBottom({ scrollTop: scrollTop + delta, scrollHeight, clientHeight });
}
