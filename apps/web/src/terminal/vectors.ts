/** The shared Noise vectors and what replaying them needs, for this folder's tests only. */
import { readFileSync } from "node:fs";

import { fromHex } from "../hex.ts";
import type { Ephemeral } from "./noise.ts";

export type Vector = {
  protocol_name: string;
  init_prologue: string;
  init_ephemeral: string;
  init_remote_static: string;
  messages: { payload: string; ciphertext: string }[];
};

export const vectors = JSON.parse(
  readFileSync(new URL("../../../../crates/protocol/noise-vectors.json", import.meta.url), "utf8"),
) as { published: Vector; weacon: Vector };

export function bytes(text: string): Uint8Array<ArrayBuffer> {
  const read = fromHex(text);
  if (read === undefined) {
    throw new RangeError("not hex");
  }
  return read;
}

/** A fixed ephemeral, as the vectors use; Web Crypto takes a private X25519 key only as PKCS #8. */
export async function fixed(privateHex: string): Promise<Ephemeral> {
  const pkcs8 = bytes(`302e020100300506032b656e04220420${privateHex}`);
  const privateKey = await crypto.subtle.importKey("pkcs8", pkcs8, { name: "X25519" }, false, ["deriveBits"]);
  const base = new Uint8Array(32);
  base[0] = 9;
  const point = await crypto.subtle.importKey("raw", base, { name: "X25519" }, false, []);
  const own = await crypto.subtle.deriveBits({ name: "X25519", public: point }, privateKey, 256);
  return { privateKey, public: new Uint8Array(own) };
}
