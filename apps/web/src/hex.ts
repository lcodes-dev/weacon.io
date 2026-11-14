/** `bytes` as lowercase hex, two digits each. */
export function hex(bytes: Uint8Array): string {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

/** The bytes `text` spells in hex, two digits each; the caller checks that it is hex. */
export function unhex(text: string): Uint8Array<ArrayBuffer> {
  return Uint8Array.from(text.match(/../g) ?? [], (pair) => Number.parseInt(pair, 16));
}
