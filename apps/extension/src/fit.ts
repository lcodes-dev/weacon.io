import { Failure, pickEnvelope } from "./core.ts";
import type { Envelope, Pick } from "./protocol.ts";

/** Must equal `weacon_protocol::MAX_FRAME`, which counts a line's newline too. */
export const MAX_FRAME = 1024 * 1024;
const MAX_HTML = 256 * 1024;
const MAX_TEXT = 4 * 1024;
const SMALLEST_SCALE = 1 / 32;

/** A rectangle in CSS pixels, from the top left of the viewport. */
export type Rect = { x: number; y: number; width: number; height: number };

/**
 * The pick cut to lengths the core takes, with the largest screenshot from `shoot(1)`, halving
 * each time, that fits in one frame; without one when none fits, `shoot` fails or there is none.
 */
export async function fit(
  base: Omit<Pick, "screenshot_png">,
  shoot?: (scale: number) => Promise<string>,
): Promise<Pick> {
  const pick = {
    request: base.request,
    url: cut(base.url, MAX_TEXT),
    title: cut(base.title, MAX_TEXT),
    selector: cut(base.selector, MAX_TEXT),
    html: cut(base.html, MAX_HTML),
  };
  for (let scale = 1; shoot !== undefined && scale >= SMALLEST_SCALE; scale /= 2) {
    const png = await shoot(scale).catch(() => undefined);
    if (png === undefined) {
      break;
    }
    const shot = { ...pick, screenshot_png: png };
    if (fits(shot)) {
      return shot;
    }
  }
  if (fits(pick)) {
    return pick;
  }
  throw new Failure("extension.pick_too_large");
}

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

// The core's JSON parser refuses half a surrogate pair, which a page's own text can hold.
function cut(text: string, length: number): string {
  return text.slice(0, length).toWellFormed();
}

function clamp(value: number, max: number): number {
  return Math.min(Math.max(value, 0), max);
}

function fits(pick: Pick): boolean {
  return frameSize(pickEnvelope(pick)) < MAX_FRAME;
}

function frameSize(envelope: Envelope): number {
  return new TextEncoder().encode(JSON.stringify(envelope)).length;
}
