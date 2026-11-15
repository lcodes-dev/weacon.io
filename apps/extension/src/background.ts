import { ME } from "./build.ts";
import { HOST, report } from "./core.ts";

// The click that opens the panel also lets the extension into the tab, for the picker.
chrome.action.onClicked.addListener((tab) => {
  if (tab.windowId !== undefined) {
    chrome.sidePanel.open({ windowId: tab.windowId }).catch(() => tell("extension.panel_failed"));
  }
});
addEventListener("error", () => tell("extension.failed"));
addEventListener("unhandledrejection", () => tell("extension.failed"));

function tell(code: string) {
  void report(chrome.runtime.connectNative(HOST), ME, code);
}
