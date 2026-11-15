import assert from "node:assert/strict";
import { test } from "node:test";

import { hex, unhex } from "../hex.ts";
import { type PinStore, Pins } from "../machines/pins.ts";
import type { MachineSummary, TerminalCommand, TerminalEvent } from "../protocol.ts";
import { SignInTerminal, type TerminalState, prologue } from "./sign-in-terminal.ts";
import { fixed, vectors } from "./vectors.ts";

const vector = vectors.weacon;
const message = (index: number) => {
  const found = vector.messages[index];
  assert.ok(found);
  return found;
};

const machine: MachineSummary = {
  id: "m-1",
  name: "Test laptop",
  os: "linux",
  key: vector.init_remote_static,
  paired_at: 0,
};

function store(pinned?: string, failing = false): PinStore {
  const keys = new Map<string, string>(pinned === undefined ? [] : [["m-1", pinned]]);
  const fail = () => Promise.reject(new Error("store unavailable"));
  return {
    get: async (id) => (failing ? fail() : keys.get(id)),
    put: async (id, key) => {
      keys.set(id, key);
    },
    delete: async (id) => {
      keys.delete(id);
    },
  };
}

function terminal(pins: Pins, online = true) {
  const sent: TerminalCommand[] = [];
  const written: string[] = [];
  const states: TerminalState[] = [];
  const session = new SignInTerminal({
    machine,
    workspace: "w-1",
    agent: "claude",
    pins,
    send: (command) => {
      sent.push(command);
      return online;
    },
    screen: {
      write: (data) => written.push(new TextDecoder().decode(data)),
      state: (state) => states.push(state),
    },
    size: { cols: 80, rows: 24 },
    terminal: "t-1",
    ephemeral: fixed(vector.init_ephemeral),
  });
  const receive = (event: TerminalEvent) => session.receive({ terminal: "t-1", workspace: "w-1", event });
  return { session, sent, written, states, receive };
}

function sealed(command: TerminalCommand | undefined): string {
  assert.ok(command && (command.action.kind === "frame" || command.action.kind === "open"));
  return command.action.kind === "frame" ? command.action.sealed : command.action.handshake;
}

test("the prologue is the core's", () => {
  assert.deepEqual(prologue("m-1", "w-1", "t-1", "claude"), unhex(vector.init_prologue));
});

test("a sign-in runs sealed to the pinned key, as the core's own vector has it", async () => {
  const { session, sent, written, states, receive } = terminal(new Pins(store(machine.key)));
  await session.start();
  assert.equal(sent.length, 1);
  assert.equal(sent[0]?.action.kind, "open");
  assert.equal(sent[0]?.action.kind === "open" && sent[0].action.agent, "claude");
  assert.equal(sealed(sent[0]), message(0).ciphertext);
  assert.equal(sent[0]?.by, undefined);

  await receive({ kind: "accepted", handshake: message(1).ciphertext });
  assert.equal(sealed(sent[1]), message(2).ciphertext);
  await receive({ kind: "frame", sealed: message(3).ciphertext });
  assert.deepEqual(written, ["Paste the code: "]);
  await session.input(new TextEncoder().encode("y\r"));
  assert.equal(sealed(sent[2]), message(4).ciphertext);
  await receive({ kind: "frame", sealed: message(5).ciphertext });
  await receive({ kind: "closed" });
  assert.deepEqual(states.map((state) => state.kind), ["checking", "connecting", "open", "ended", "closed"]);
  assert.deepEqual(states.at(-2), { kind: "ended", signIn: "signed_in" });
  assert.deepEqual(states.at(-1), { kind: "closed" });
});

test("a long paste goes in pieces, each sealed in a frame of its own", async () => {
  const { session, sent, states, receive } = terminal(new Pins(store(machine.key)));
  await session.start();
  await receive({ kind: "accepted", handshake: message(1).ciphertext });
  const before = sent.length;

  await session.input(new Uint8Array(40_000).fill(0x61));
  await session.input(new Uint8Array());

  assert.equal(sent.length - before, 3);
  assert.ok(sent.slice(before).every((command) => command.action.kind === "frame"));
  assert.equal(states.at(-1)?.kind, "open");
});

test("what is typed goes in the order it was typed, a long paste's pieces together", async () => {
  const { session, sent, receive } = terminal(new Pins(store(machine.key)));
  await session.start();
  await receive({ kind: "accepted", handshake: message(1).ciphertext });
  const before = sent.length;

  const paste = session.input(new Uint8Array(16_385).fill(0x61));
  const enter = session.input(new TextEncoder().encode("\r\n"));
  await Promise.all([paste, enter]);

  const lengths = sent.slice(before).map((command) => sealed(command).length);
  assert.equal(lengths.length, 3);
  // The paste's last byte, then the two of Enter, each as hex.
  assert.deepEqual([lengths[2]! - lengths[1]!], [4]);
});

test("a connection lost while the handshake finishes leaves the terminal closed", async () => {
  const { session, states, receive } = terminal(new Pins(store(machine.key)));
  await session.start();
  const subtle = crypto.subtle;
  const derive = subtle.deriveBits.bind(subtle);
  // The core's reply is in, and the connection goes as its keys are worked out.
  subtle.deriveBits = (...args: Parameters<SubtleCrypto["deriveBits"]>) => {
    session.lost();
    return derive(...args);
  };
  try {
    await receive({ kind: "accepted", handshake: message(1).ciphertext });
  } finally {
    delete (subtle as Partial<SubtleCrypto>).deriveBits;
  }

  assert.deepEqual(states.at(-1), { kind: "closed", problem: "offline" });
  assert.ok(!states.some((state) => state.kind === "open"));
});

test("a changed key, or a pin that can't be read, sends nothing", async () => {
  for (const [pins, problem] of [
    [new Pins(store("00".repeat(32))), "key_changed"],
    [new Pins(store(undefined, true)), "key_unchecked"],
  ] as const) {
    const { session, sent, states } = terminal(pins);
    await session.start();
    assert.deepEqual(sent, []);
    assert.deepEqual(states.at(-1), { kind: "closed", problem });
  }
});

test("an unpinned machine sends nothing and is never pinned here", async () => {
  const pinned: string[] = [];
  const pins = new Pins({ ...store(), put: async (id) => void pinned.push(id) });
  const { session, sent, states } = terminal(pins);
  await session.start();
  await session.start();
  assert.deepEqual(sent, []);
  assert.deepEqual(pinned, []);
  assert.deepEqual(states.at(-1), { kind: "closed", problem: "unpinned" });
});

test("anything that doesn't check out closes the terminal, which then shows nothing", async () => {
  const { session, sent, written, states, receive } = terminal(new Pins(store(machine.key)));
  await session.start();
  await receive({ kind: "accepted", handshake: message(1).ciphertext });
  const tampered = unhex(message(3).ciphertext);
  tampered[0] = (tampered[0] ?? 0) ^ 1;
  await receive({ kind: "frame", sealed: hex(tampered) });
  assert.deepEqual(states.at(-1), { kind: "closed", problem: "broken" });
  assert.equal(sent.at(-1)?.action.kind, "close");

  const count = sent.length;
  await receive({ kind: "frame", sealed: message(3).ciphertext });
  await session.input(new TextEncoder().encode("y\r"));
  assert.deepEqual(written, []);
  assert.equal(sent.length, count);
});

test("a second reply, a reply before the handshake, or a frame before the reply breaks it", async () => {
  for (const events of [
    [{ kind: "frame", sealed: message(3).ciphertext }],
    [
      { kind: "accepted", handshake: message(1).ciphertext },
      { kind: "accepted", handshake: message(1).ciphertext },
    ],
  ] as const) {
    const { session, states, receive } = terminal(new Pins(store(machine.key)));
    await session.start();
    for (const event of events) {
      await receive(event);
    }
    assert.deepEqual(states.at(-1), { kind: "closed", problem: "broken" });
  }
});

test("the core's refusal closes the terminal with its code, and others' updates are ignored", async () => {
  const { session, states, receive } = terminal(new Pins(store(machine.key)));
  await session.start();
  await session.receive({ terminal: "t-2", workspace: "w-1", event: { kind: "closed" } });
  await session.receive({ terminal: "t-1", workspace: "w-2", event: { kind: "closed" } });
  assert.equal(states.at(-1)?.kind, "connecting");
  await receive({ kind: "closed", code: "terminal.busy" });
  assert.deepEqual(states.at(-1), { kind: "closed", problem: "terminal.busy" });
});

test("updates that come at once are taken in order", async () => {
  const { session, written, receive } = terminal(new Pins(store(machine.key)));
  await session.start();
  await Promise.all([
    receive({ kind: "accepted", handshake: message(1).ciphertext }),
    receive({ kind: "frame", sealed: message(3).ciphertext }),
  ]);
  assert.deepEqual(written, ["Paste the code: "]);
});

test("a terminal closed while it checks or opens sends nothing, and opens only once", async () => {
  const { session, sent, states } = terminal(new Pins(store(machine.key)));
  const starting = session.start();
  session.close();
  await starting;
  assert.deepEqual(sent, []);
  assert.deepEqual(states.at(-1), { kind: "closed" });

  const twice = terminal(new Pins(store(machine.key)));
  await Promise.all([twice.session.start(), twice.session.start()]);
  assert.deepEqual(twice.sent.map((command) => command.action.kind), ["open"]);
});

test("closing sends one close and nothing after", async () => {
  const { session, sent, receive } = terminal(new Pins(store(machine.key)));
  await session.start();
  await receive({ kind: "accepted", handshake: message(1).ciphertext });
  session.close();
  session.close();
  await session.resize({ cols: 100, rows: 30 });
  assert.deepEqual(
    sent.map((command) => command.action.kind),
    ["open", "frame", "close"],
  );
});

test("a terminal whose connection is lost closes as offline, once, and sends nothing more", async () => {
  const { session, states, sent } = terminal(new Pins(store(machine.key)));
  await session.start();
  session.lost();
  await session.receive({ terminal: "t-1", workspace: "w-1", event: { kind: "closed" } });
  session.close();

  assert.deepEqual(states.filter((state) => state.kind === "closed"), [{ kind: "closed", problem: "offline" }]);
  assert.deepEqual(
    sent.map((command) => command.action.kind),
    ["open"],
  );
});

test("a connection that can't take a command ends the terminal", async () => {
  const { session, states } = terminal(new Pins(store(machine.key)), false);
  await session.start();
  assert.deepEqual(states.at(-1), { kind: "closed", problem: "offline" });
});
