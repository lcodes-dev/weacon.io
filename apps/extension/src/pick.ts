import { type Component, componentIn, componentOf } from "./component.ts";
import { Failure } from "./core.ts";
import { fit } from "./fit.ts";
import { isWebPage } from "./page.ts";
import type { Picked, Picker } from "./picker.ts";
import type { Pick } from "./protocol.ts";
import { base64, crop, type Rect } from "./screenshot.ts";

/**
 * Lets the person pick an element on `tab`, and gives it with a screenshot, cut to fit one frame;
 * `undefined` when they cancelled. Chrome lets the extension into the tab only after the person
 * clicked its button there, through `activeTab`.
 */
export async function pickOn(tab: chrome.tabs.Tab): Promise<Pick | undefined> {
  // Chrome shows the tab's address only once the person let the extension in.
  if (tab.id === undefined || tab.url === undefined) {
    throw new Failure("extension.page_not_allowed");
  }
  if (!isWebPage(tab.url)) {
    throw new Failure("extension.page_not_web");
  }
  const target = { tabId: tab.id };
  await chrome.scripting.executeScript({ target, files: ["picker.js"] }).catch(() => {
    throw new Failure("extension.page_not_allowed");
  });
  const [injection] = await chrome.scripting.executeScript({
    target,
    func: () => (globalThis as Picker).weaconPick?.() ?? null,
  });
  const picked = injection?.result as Picked | null | undefined;
  if (!picked) {
    return undefined;
  }
  const { rect, ratio, ...page } = picked;
  // Without a screenshot or a component, the rest of the pick still goes.
  const shoot = await shooter(tab.id, tab.windowId, rect, ratio).catch(() => undefined);
  const component = await named(tab.id, page.selector).catch(() => ({}));
  return fit({ request: crypto.randomUUID(), ...page, ...component }, shoot);
}

async function named(tabId: number, selector: string): Promise<Component> {
  const [injection] = await chrome.scripting.executeScript({
    target: { tabId },
    world: "MAIN",
    func: componentOf,
    args: [selector],
  });
  return componentIn(injection?.result);
}

// Captures the tab and gives the part showing `rect` as a base64 PNG at any scale. Chrome
// captures whichever tab is showing, so the capture counts only if the picked one shows throughout.
async function shooter(
  tabId: number,
  windowId: number,
  rect: Rect,
  ratio: number,
): Promise<(scale: number) => Promise<string>> {
  const showing = async () => (await chrome.tabs.query({ active: true, windowId }))[0]?.id === tabId;
  const before = await showing();
  const capture = await chrome.tabs.captureVisibleTab(windowId, { format: "png" });
  if (!before || !(await showing())) {
    throw new Failure("extension.capture_failed");
  }
  const image = await createImageBitmap(await (await fetch(capture)).blob());
  const part = crop(rect, ratio, image.width, image.height);
  return async (scale) => {
    const width = Math.max(1, Math.round(part.width * scale));
    const height = Math.max(1, Math.round(part.height * scale));
    const canvas = new OffscreenCanvas(width, height);
    const context = canvas.getContext("2d");
    if (context === null) {
      throw new Failure("extension.capture_failed");
    }
    context.drawImage(image, part.x, part.y, part.width, part.height, 0, 0, width, height);
    return base64(new Uint8Array(await (await canvas.convertToBlob({ type: "image/png" })).arrayBuffer()));
  };
}
