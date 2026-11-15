/**
 * Applies the rules xterm writes into `<style>` elements under `host` through a constructed
 * stylesheet, as the server's policy blocks inline styles but not this; until the returned
 * function is called.
 */
export function adoptStyles(host: HTMLElement): () => void {
  const sheet = new CSSStyleSheet();
  document.adoptedStyleSheets = [...document.adoptedStyleSheets, sheet];
  const sync = () => sheet.replaceSync([...host.querySelectorAll("style")].map((style) => style.textContent).join("\n"));
  const styled = (node: Node) => node.nodeName === "STYLE" || node.parentNode?.nodeName === "STYLE";
  // The rows change all the time; only a change to a style needs the sheet written again.
  const observer = new MutationObserver((records) => {
    if (records.some((record) => styled(record.target) || [...record.addedNodes].some(styled))) {
      sync();
    }
  });
  observer.observe(host, { childList: true, characterData: true, subtree: true });
  sync();
  return () => {
    observer.disconnect();
    document.adoptedStyleSheets = document.adoptedStyleSheets.filter((adopted) => adopted !== sheet);
  };
}
