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
  return selection !== null && !selection.isCollapsed && selection.rangeCount > 0 && selection.getRangeAt(0).intersectsNode(container);
}
