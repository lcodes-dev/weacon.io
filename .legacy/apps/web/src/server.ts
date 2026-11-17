/** The fake server a page opened from a file names with `?server=`, as in tests; otherwise the page is the server's own. */
export function fakeServerUrl(location: { protocol: string; search: string }): string | undefined {
  const given = new URLSearchParams(location.search).get("server");
  return location.protocol === "file:" && given !== null ? given : undefined;
}
