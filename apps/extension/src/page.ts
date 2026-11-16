/** Whether `url` is a web page, served over http or https: the only kind the picker works on. */
export function isWebPage(url: string): boolean {
  if (!URL.canParse(url)) {
    return false;
  }
  const { protocol } = new URL(url);
  return protocol === "http:" || protocol === "https:";
}
