/** Where the server's WebSocket is: this page's host, or `?server=` when the page is a file, as in tests. */
export function serverUrl(location: { protocol: string; host: string; search: string }): string {
  const given = new URLSearchParams(location.search).get("server");
  if (location.protocol === "file:" && given !== null) {
    return given;
  }
  return `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/ws`;
}
