import type { RequestId } from "./protocol.ts";

/** Whether `text` has the form of the protocol's ids (`token_id!`): 1–64 ASCII letters, digits, `-` or `_`. */
export function isId(text: string): boolean {
  return /^[A-Za-z0-9_-]{1,64}$/.test(text);
}

/** A request id made of `bytes` in hex. */
export function requestId(bytes: Uint8Array): RequestId {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

/** A new request id, from 16 random bytes. */
export function newRequestId(): RequestId {
  return requestId(crypto.getRandomValues(new Uint8Array(16)));
}
