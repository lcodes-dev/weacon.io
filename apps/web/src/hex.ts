/** `bytes` as lowercase hex, two digits each. */
export function hex(bytes: Uint8Array): string {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

/** The bytes `text` spells in lowercase hex; `undefined` when it spells none. */
export function fromHex(text: string): Uint8Array<ArrayBuffer> | undefined {
  if (!/^(?:[0-9a-f]{2})*$/.test(text)) {
    return undefined;
  }
  return Uint8Array.from(text.match(/../g) ?? [], (pair) => Number.parseInt(pair, 16));
}
