import { Failure } from "./core.ts";

/** A rectangle in CSS pixels, from the top left of the viewport. */
export type Rect = { x: number; y: number; width: number; height: number };

/** The part of a `width` × `height` capture, in its pixels, that shows `rect`. */
export function crop(rect: Rect, ratio: number, width: number, height: number): Rect {
  const left = clamp(Math.round(rect.x * ratio), width);
  const top = clamp(Math.round(rect.y * ratio), height);
  const right = clamp(Math.round((rect.x + rect.width) * ratio), width);
  const bottom = clamp(Math.round((rect.y + rect.height) * ratio), height);
  if (right <= left || bottom <= top) {
    throw new Failure("extension.element_hidden");
  }
  return { x: left, y: top, width: right - left, height: bottom - top };
}

/** `bytes` as base64, in chunks small enough to pass as arguments. */
export function base64(bytes: Uint8Array): string {
  let binary = "";
  for (let at = 0; at < bytes.length; at += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(at, at + 0x8000));
  }
  return btoa(binary);
}

function clamp(value: number, max: number): number {
  return Math.min(Math.max(value, 0), max);
}
