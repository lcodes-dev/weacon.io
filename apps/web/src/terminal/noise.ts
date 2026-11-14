/**
 * The browser's end of a sign-in terminal's seal: Noise_NK_25519_AESGCM_SHA256 as the
 * initiator, on Web Crypto alone. The core's static key is the one pinned at pairing, never
 * one from the wire. After any failure a channel refuses everything, so the terminal closes.
 */

export const PROTOCOL = "Noise_NK_25519_AESGCM_SHA256";
/** The largest Noise message. */
export const NOISE_MAX = 65535;
const TAG = 16;
/** The largest plaintext one sealed message holds. */
export const SEALED_MAX = NOISE_MAX - TAG;
const KEY = 32;
const NONCE_LIMIT = 2n ** 64n - 1n;

const subtle = globalThis.crypto.subtle;

type Bytes = Uint8Array<ArrayBuffer>;

/** An X25519 key pair whose private half can't be exported. */
export type Ephemeral = { privateKey: CryptoKey; public: Bytes };

/** A fresh ephemeral key pair. */
export async function ephemeral(): Promise<Ephemeral> {
  const pair = await subtle.generateKey({ name: "X25519" }, false, ["deriveBits"]);
  return { privateKey: pair.privateKey, public: new Uint8Array(await subtle.exportKey("raw", pair.publicKey)) };
}

/** The handshake's first message, sealed to `remote` under `prologue`, carrying `payload`. */
export async function initiate(
  remote: Bytes,
  prologue: Bytes,
  payload: Bytes,
  own: Promise<Ephemeral> = ephemeral(),
): Promise<{ message: Bytes; pending: Pending }> {
  return sealing(async () => {
    const e = await own;
    if (remote.length !== KEY) {
      throw new SealError();
    }
    const state = new Symmetric();
    await state.mixHash(prologue);
    await state.mixHash(remote);
    await state.mixHash(e.public);
    await state.mixKey(await dh(e.privateKey, remote));
    const message = concat(e.public, await state.encryptAndHash(payload));
    if (message.length > NOISE_MAX) {
      throw new SealError();
    }
    return { message, pending: new Pending(state, e.privateKey) };
  });
}

/** A handshake waiting for the core's reply. */
export class Pending {
  #state: Symmetric | undefined;
  readonly #own: CryptoKey;

  constructor(state: Symmetric, own: CryptoKey) {
    this.#state = state;
    this.#own = own;
  }

  /** The channel once the core's `reply` checks out, and the reply's payload. */
  finish(reply: Bytes): Promise<{ channel: Channel; payload: Bytes }> {
    const state = this.#state;
    this.#state = undefined;
    return sealing(async () => {
      if (state === undefined || reply.length < KEY + TAG || reply.length > NOISE_MAX) {
        throw new SealError();
      }
      const re = reply.slice(0, KEY);
      await state.mixHash(re);
      await state.mixKey(await dh(this.#own, re));
      const payload = await state.decryptAndHash(reply.slice(KEY));
      const [send, receive] = await state.split();
      return { channel: new Channel(send, receive), payload };
    });
  }
}

/** One terminal's channel once the handshake is done. */
export class Channel {
  readonly #send: Cipher;
  readonly #receive: Cipher;
  #broken = false;
  #last: Promise<unknown> = Promise.resolve();

  constructor(send: Cipher, receive: Cipher) {
    this.#send = send;
    this.#receive = receive;
  }

  /** `plaintext` as the next message to send. */
  seal(plaintext: Bytes): Promise<Bytes> {
    return this.#guard(plaintext.length > SEALED_MAX, () => this.#send.encrypt(new Uint8Array(), plaintext));
  }

  /** Refuses everything from now on. */
  close(): void {
    this.#broken = true;
  }

  /** The plaintext of the next message received. */
  open(message: Bytes): Promise<Bytes> {
    return this.#guard(message.length > NOISE_MAX || message.length < TAG, () =>
      this.#receive.decrypt(new Uint8Array(), message),
    );
  }

  // One step at a time, in call order, so nonces follow the calls and nothing passes after
  // a failure.
  #guard(refused: boolean, step: () => Promise<Bytes>): Promise<Bytes> {
    const run = this.#last.then(async () => {
      if (this.#broken || refused) {
        this.#broken = true;
        throw new SealError();
      }
      try {
        return await sealing(step);
      } catch (error) {
        this.#broken = true;
        throw error;
      }
    });
    this.#last = run.catch(() => undefined);
    return run;
  }
}

/** Any failure of the seal; it says nothing more, so nothing more can leak. */
export class SealError extends Error {
  constructor() {
    super("The terminal's seal failed");
  }
}

async function sealing<T>(run: () => Promise<T>): Promise<T> {
  try {
    return await run();
  } catch {
    throw new SealError();
  }
}

/** AES-256-GCM with Noise's nonces: four zero bytes, then the counter in big-endian. */
class Cipher {
  readonly #key: CryptoKey;
  #n = 0n;

  constructor(key: CryptoKey) {
    this.#key = key;
  }

  encrypt(ad: Bytes, plaintext: Bytes): Promise<Bytes> {
    return this.#step(ad, plaintext, (params, key, data) => subtle.encrypt(params, key, data));
  }

  decrypt(ad: Bytes, ciphertext: Bytes): Promise<Bytes> {
    return this.#step(ad, ciphertext, (params, key, data) => subtle.decrypt(params, key, data));
  }

  async #step(
    ad: Bytes,
    data: Bytes,
    run: (params: AesGcmParams, key: CryptoKey, data: Bytes) => Promise<ArrayBuffer>,
  ): Promise<Bytes> {
    if (this.#n >= NONCE_LIMIT) {
      throw new SealError();
    }
    const iv = new Uint8Array(12);
    new DataView(iv.buffer).setBigUint64(4, this.#n);
    this.#n += 1n;
    return new Uint8Array(await run({ name: "AES-GCM", iv, additionalData: ad, tagLength: 128 }, this.#key, data));
  }
}

/** Noise's symmetric state: the chaining key, the handshake hash and the cipher. */
class Symmetric {
  #ck: Bytes;
  #h: Bytes;
  #cipher: Cipher | undefined;

  constructor() {
    const name = new TextEncoder().encode(PROTOCOL);
    this.#h = new Uint8Array(32);
    this.#h.set(name);
    this.#ck = this.#h.slice();
  }

  async mixHash(data: Bytes): Promise<void> {
    this.#h = new Uint8Array(await subtle.digest("SHA-256", concat(this.#h, data)));
  }

  async mixKey(ikm: Bytes): Promise<void> {
    const [ck, key] = await hkdf(this.#ck, ikm);
    this.#ck = ck;
    this.#cipher = new Cipher(await aesKey(key));
  }

  async encryptAndHash(plaintext: Bytes): Promise<Bytes> {
    if (this.#cipher === undefined) {
      throw new SealError();
    }
    const ciphertext = await this.#cipher.encrypt(this.#h, plaintext);
    await this.mixHash(ciphertext);
    return ciphertext;
  }

  async decryptAndHash(ciphertext: Bytes): Promise<Bytes> {
    if (this.#cipher === undefined) {
      throw new SealError();
    }
    const plaintext = await this.#cipher.decrypt(this.#h, ciphertext);
    await this.mixHash(ciphertext);
    return plaintext;
  }

  /** The initiator's sending cipher, then its receiving one. */
  async split(): Promise<[Cipher, Cipher]> {
    const [send, receive] = await hkdf(this.#ck, new Uint8Array());
    this.#ck.fill(0);
    return [new Cipher(await aesKey(send)), new Cipher(await aesKey(receive))];
  }
}

// Noise's HKDF with two outputs is RFC 5869's with `ck` as the salt, no info and 64 bytes.
async function hkdf(ck: Bytes, ikm: Bytes): Promise<[Bytes, Bytes]> {
  const material = await subtle.importKey("raw", ikm, "HKDF", false, ["deriveBits"]);
  ikm.fill(0);
  const out = new Uint8Array(
    await subtle.deriveBits({ name: "HKDF", hash: "SHA-256", salt: ck, info: new Uint8Array() }, material, 512),
  );
  return [out.slice(0, KEY), out.slice(KEY)];
}

async function aesKey(raw: Bytes): Promise<CryptoKey> {
  const key = await subtle.importKey("raw", raw, "AES-GCM", false, ["encrypt", "decrypt"]);
  raw.fill(0);
  return key;
}

// Web Crypto refuses a low-order point, whose secret would be all zeros.
async function dh(own: CryptoKey, remote: Bytes): Promise<Bytes> {
  const key = await subtle.importKey("raw", remote, { name: "X25519" }, false, []);
  return new Uint8Array(await subtle.deriveBits({ name: "X25519", public: key }, own, 256));
}

function concat(one: Bytes, two: Bytes): Bytes {
  const joined = new Uint8Array(one.length + two.length);
  joined.set(one);
  joined.set(two, one.length);
  return joined;
}
