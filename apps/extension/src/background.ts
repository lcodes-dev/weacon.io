import { Failure, HOST, app, deliver, report } from "./core.ts";
import { reported, title } from "./feedback.ts";
import { base64, crop, fit, type Rect } from "./fit.ts";
import { isLocal } from "./local.ts";
import type { Picked, Picker } from "./picker.ts";

const ME = app(chrome.runtime.getManifest());
const picking = new Set<number>();

chrome.action.onClicked.addListener((tab) => {
  if (tab.id === undefined || picking.has(tab.id)) {
    return;
  }
  const tabId = tab.id;
  picking.add(tabId);
  show(tabId, "", "Pick an element to send to Weacon");
  pickIn(tab, tabId)
    .then(
      (sent) => sent && show(tabId, "✓", "Sent to Weacon"),
      (error: unknown) => {
        const code = error instanceof Failure ? error.code : "extension.failed";
        show(tabId, "!", title(code));
        tell(code);
      },
    )
    .finally(() => picking.delete(tabId));
});
addEventListener("error", () => tell("extension.failed"));
addEventListener("unhandledrejection", () => tell("extension.failed"));

// Whether an element was picked and sent; false when the person cancelled.
async function pickIn(tab: chrome.tabs.Tab, tabId: number): Promise<boolean> {
  if (tab.url === undefined || !isLocal(tab.url)) {
    throw new Failure("extension.page_not_local");
  }
  const target = { tabId };
  await chrome.scripting.executeScript({ target, files: ["picker.js"] });
  const [injection] = await chrome.scripting.executeScript({
    target,
    func: () => (globalThis as Picker).weaconPick?.() ?? null,
  });
  const picked = injection?.result as Picked | null | undefined;
  if (!picked) {
    return false;
  }
  const { rect, ratio, ...page } = picked;
  // Without a screenshot, the rest of the pick still goes.
  const shoot = await shooter(tab.windowId, rect, ratio).catch(() => undefined);
  const pick = await fit({ request: crypto.randomUUID(), ...page }, shoot);
  await deliver(chrome.runtime.connectNative(HOST), ME, pick);
  return true;
}

// Captures the tab and gives the part showing `rect` as a base64 PNG at any scale.
async function shooter(windowId: number, rect: Rect, ratio: number): Promise<(scale: number) => Promise<string>> {
  const capture = await chrome.tabs.captureVisibleTab(windowId, { format: "png" });
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

function show(tabId: number, text: string, label: string) {
  void chrome.action.setBadgeText({ tabId, text });
  void chrome.action.setTitle({ tabId, title: label });
}

function tell(code: string) {
  if (reported(code)) {
    void report(chrome.runtime.connectNative(HOST), ME, code);
  }
}
