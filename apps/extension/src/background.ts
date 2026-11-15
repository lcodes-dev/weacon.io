import { tell } from "./tell.ts";

// The click that opens the panel also lets the extension into the tab, for the picker.
chrome.action.onClicked.addListener((tab) => {
  chrome.sidePanel.open({ windowId: tab.windowId }).catch(() => tell("extension.panel_failed"));
});
addEventListener("error", () => tell("extension.failed"));
addEventListener("unhandledrejection", () => tell("extension.failed"));
