/** Whether `url` is a page served from this machine, the only kind the picker works on. */
export function isLocal(url: string): boolean {
  if (!URL.canParse(url)) {
    return false;
  }
  const { protocol, hostname } = new URL(url);
  const local = ["localhost", "127.0.0.1", "[::1]"].includes(hostname) || hostname.endsWith(".localhost");
  return (protocol === "http:" || protocol === "https:") && local;
}
