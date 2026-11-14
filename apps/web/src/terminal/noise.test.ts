import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import { type Channel, type Ephemeral, NOISE_MAX, PROTOCOL, SEALED_MAX, SealError, ephemeral, initiate } from "./noise.ts";

type Vector = {
  protocol_name: string;
  init_prologue: string;
  init_ephemeral: string;
  init_remote_static: string;
  messages: { payload: string; ciphertext: string }[];
};

const vectors = JSON.parse(
  readFileSync(new URL("../../../../crates/protocol/noise-vectors.json", import.meta.url), "utf8"),
) as { published: Vector; weacon: Vector };

function bytes(hex: string): Uint8Array<ArrayBuffer> {
  return Uint8Array.from(hex.match(/../g) ?? [], (pair) => parseInt(pair, 16));
}

function hex(data: Uint8Array): string {
  return Array.from(data, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

// A fixed ephemeral, as the vectors use; Web Crypto takes a private X25519 key only as PKCS #8.
async function fixed(privateHex: string): Promise<Ephemeral> {
  const pkcs8 = bytes(`302e020100300506032b656e04220420${privateHex}`);
  const privateKey = await crypto.subtle.importKey("pkcs8", pkcs8, { name: "X25519" }, false, ["deriveBits"]);
  const base = new Uint8Array(32);
  base[0] = 9;
  const point = await crypto.subtle.importKey("raw", base, { name: "X25519" }, false, []);
  const own = await crypto.subtle.deriveBits({ name: "X25519", public: point }, privateKey, 256);
  return { privateKey, public: new Uint8Array(own) };
}

async function handshake(vector: Vector): Promise<Channel> {
  const [first, reply] = vector.messages;
  assert.ok(first && reply);
  const { message, pending } = await initiate(
    bytes(vector.init_remote_static),
    bytes(vector.init_prologue),
    bytes(first.payload),
    fixed(vector.init_ephemeral),
  );
  assert.equal(hex(message), first.ciphertext);
  const finished = await pending.finish(bytes(reply.ciphertext));
  assert.equal(hex(finished.payload), reply.payload);
  return finished.channel;
}

async function refused(promise: Promise<unknown>): Promise<void> {
  await assert.rejects(promise, (error) => error instanceof SealError && error.message === "The terminal's seal failed");
}

for (const name of ["published", "weacon"] as const) {
  test(`the browser's end matches the ${name} vector`, async () => {
    const vector = vectors[name];
    const channel = await handshake(vector);
    assert.equal(vector.protocol_name, PROTOCOL);
    for (const [index, message] of vector.messages.slice(2).entries()) {
      if (index % 2 === 0) {
        assert.equal(hex(await channel.seal(bytes(message.payload))), message.ciphertext);
      } else {
        assert.equal(hex(await channel.open(bytes(message.ciphertext))), message.payload);
      }
    }
  });
}

test("a reply that doesn't check out, or a second one, is refused", async () => {
  const vector = vectors.weacon;
  const [first, reply] = vector.messages;
  assert.ok(first && reply);
  const start = () =>
    initiate(bytes(vector.init_remote_static), bytes(vector.init_prologue), bytes(first.payload), fixed(vector.init_ephemeral));
  const tampered = bytes(reply.ciphertext);
  tampered[40] = (tampered[40] ?? 0) ^ 1;
  await refused((await start()).pending.finish(tampered));
  await refused((await start()).pending.finish(bytes(reply.ciphertext).slice(0, 47)));
  await refused((await start()).pending.finish(new Uint8Array(32 + 16)));

  const { pending } = await start();
  await pending.finish(bytes(reply.ciphertext));
  await refused(pending.finish(bytes(reply.ciphertext)));
});

test("a reply made for another prologue or key is refused", async () => {
  const vector = vectors.weacon;
  const [first, reply] = vector.messages;
  assert.ok(first && reply);
  const other = bytes(vector.init_prologue);
  other[other.length - 1] = (other[other.length - 1] ?? 0) ^ 1;
  const moved = await initiate(bytes(vector.init_remote_static), other, bytes(first.payload), fixed(vector.init_ephemeral));
  await refused(moved.pending.finish(bytes(reply.ciphertext)));
  const elsewhere = await initiate((await ephemeral()).public, bytes(vector.init_prologue), bytes(first.payload));
  await refused(elsewhere.pending.finish(bytes(reply.ciphertext)));
  await refused(initiate(new Uint8Array(31), bytes(vector.init_prologue), new Uint8Array()));
  await refused(initiate(new Uint8Array(32), bytes(vector.init_prologue), new Uint8Array()));
});

test("a tampered, replayed or reordered frame breaks the channel for good", async () => {
  const vector = vectors.published;
  const [, , , fourth, , sixth] = vector.messages;
  assert.ok(fourth && sixth);

  let channel = await handshake(vector);
  const tampered = bytes(fourth.ciphertext);
  tampered[0] = (tampered[0] ?? 0) ^ 1;
  await refused(channel.open(tampered));
  await refused(channel.open(bytes(fourth.ciphertext)));
  await refused(channel.seal(new Uint8Array()));

  channel = await handshake(vector);
  await refused(channel.open(bytes(sixth.ciphertext)));

  channel = await handshake(vector);
  await channel.open(bytes(fourth.ciphertext));
  await refused(channel.open(bytes(fourth.ciphertext)));
});

test("a closed channel or a message over the Noise limit is refused", async () => {
  let channel = await handshake(vectors.weacon);
  channel.close();
  await refused(channel.seal(new Uint8Array()));

  channel = await handshake(vectors.weacon);
  assert.equal((await channel.seal(new Uint8Array(SEALED_MAX))).length, NOISE_MAX);
  await refused(channel.seal(new Uint8Array(SEALED_MAX + 1)));
  channel = await handshake(vectors.weacon);
  await refused(channel.open(new Uint8Array(NOISE_MAX + 1)));
  channel = await handshake(vectors.weacon);
  await refused(channel.open(new Uint8Array(15)));
});

test("frames sealed at once still each get their own nonce, in call order", async () => {
  const vector = vectors.weacon;
  const third = vector.messages[2];
  assert.ok(third);
  const channel = await handshake(vector);
  const [one, two] = await Promise.all([channel.seal(bytes(third.payload)), channel.seal(bytes(third.payload))]);
  assert.equal(hex(one), third.ciphertext);
  assert.notEqual(hex(two), third.ciphertext);
});

test("a fresh ephemeral can't be exported", async () => {
  const own = await ephemeral();
  assert.equal(own.public.length, 32);
  assert.equal(own.privateKey.extractable, false);
  await assert.rejects(crypto.subtle.exportKey("pkcs8", own.privateKey));
});

test("the limit is the protocol's", () => {
  const schema = JSON.parse(readFileSync(new URL("../../../../crates/protocol/schema.json", import.meta.url), "utf8")) as {
    $defs: { Bytes: { maxLength: number } };
  };
  assert.equal(schema.$defs.Bytes.maxLength, 2 * NOISE_MAX);
});
