import { hex } from "./hex.ts";
import type { RequestId, TerminalId } from "./protocol.ts";

/** Whether `text` has the form of the protocol's ids (`token_id!`): 1–64 ASCII letters, digits, `-` or `_`. */
export function isId(text: string): boolean {
  return /^[A-Za-z0-9_-]{1,64}$/.test(text);
}

/** A new id for a request or a terminal: 16 random bytes in hex. */
export function newId(): RequestId & TerminalId {
  return hex(crypto.getRandomValues(new Uint8Array(16)));
}
