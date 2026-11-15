import { Failure, pickEnvelope } from "./core.ts";
import type { Envelope, Pick } from "./protocol.ts";

/** Must equal `weacon_protocol::MAX_FRAME`, which counts a line's newline too. */
export const MAX_FRAME = 1024 * 1024;
const MAX_HTML = 256 * 1024;
const MAX_TEXT = 4 * 1024;
const SMALLEST_SCALE = 1 / 32;

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

// The core's JSON parser refuses half a surrogate pair, which a page's own text can hold.
function cut(text: string, length: number): string {
  return text.slice(0, length).toWellFormed();
}

function fits(pick: Pick): boolean {
  return frameSize(pickEnvelope(pick)) < MAX_FRAME;
}

function frameSize(envelope: Envelope): number {
  return new TextEncoder().encode(JSON.stringify(envelope)).length;
}
