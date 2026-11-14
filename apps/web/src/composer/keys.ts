/** The parts of a key press that decide what it does in the message box. */
export type Key = Pick<KeyboardEvent, "key" | "shiftKey" | "isComposing" | "keyCode">;

/** Whether a key press belongs to an input method's composition, and so to nothing else. */
export function composing(key: Key): boolean {
  // Safari ends the composition before the Enter that commits it, which then reports key code 229.
  return key.isComposing || key.keyCode === 229;
}

/** Whether a key press sends the message: Enter without Shift, outside a composition. */
export function sends(key: Key): boolean {
  return !composing(key) && key.key === "Enter" && !key.shiftKey;
}

/** What a key does while the picker is open: pick, close it, or move to an option. */
export function picker(key: Key, count: number, active: number): "pick" | "close" | number | undefined {
  if (key.shiftKey) {
    return undefined;
  }
  switch (key.key) {
    case "Enter":
    case "Tab":
      return "pick";
    case "Escape":
      return "close";
    case "ArrowDown":
      return (active + 1) % count;
    case "ArrowUp":
      return (active + count - 1) % count;
    default:
      return undefined;
  }
}
