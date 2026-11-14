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

/** The parts of a key press that say whether it scrolls the reader up. */
export type ScrollKey = { key: string; shiftKey: boolean };

/** Whether a key press, outside anything editable, scrolls the reader up and away from the bottom. */
export function leavesBottom({ key, shiftKey }: ScrollKey, editable: boolean): boolean {
  return !editable && (["ArrowUp", "PageUp", "Home"].includes(key) || (key === " " && shiftKey));
}

/**
 * Whether the view follows after a scroll: at the bottom it does; while the reader holds the
 * scrollbar or a touch, the scroll is theirs and leaving the bottom stops it; any other scroll,
 * such as the browser keeping rows in place as they reach their height, leaves it as it was.
 */
export function followsAfter(following: boolean, bottom: boolean, held: boolean): boolean {
  return bottom || (following && !held);
}
