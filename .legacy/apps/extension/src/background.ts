// The click that opens the panel also lets the extension into the tab, for the picker.
chrome.action.onClicked.addListener((tab) => {
  void chrome.sidePanel.open({ windowId: tab.windowId });
});
