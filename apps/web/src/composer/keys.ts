/** The parts of a key press that decide what it does in the message box. */
export type Key = Pick<KeyboardEvent, "key" | "shiftKey" | "isComposing" | "keyCode">;

/** Whether a key press sends the message: Enter without Shift, unless an input method is composing. */
export function sends(key: Key): boolean {
  // Safari ends the composition before the Enter that commits it, which then reports key code 229.
  const composing = key.isComposing || key.keyCode === 229;
  return !composing && key.key === "Enter" && !key.shiftKey;
}
