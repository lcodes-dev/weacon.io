import assert from "node:assert/strict";
import { test } from "node:test";

import { hex } from "../hex.ts";
import type { MachineSummary, TerminalCommand, TerminalEvent } from "../protocol.ts";
import { SignInTerminal, type TerminalState } from "./sign-in-terminal.ts";

const machine: MachineSummary = { id: "m-1", name: "Test laptop", os: "linux", paired_at: 0 };
const text = (shown: string) => hex(new TextEncoder().encode(shown));

function terminal(online = true) {
  const sent: TerminalCommand[] = [];
  const written: string[] = [];
  const states: TerminalState[] = [];
  const session = new SignInTerminal({
    machine,
    workspace: "w-1",
    agent: "claude",
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
  });
  const receive = (event: TerminalEvent) => session.receive({ terminal: "t-1", workspace: "w-1", event });
  return { session, sent, written, states, receive };
}

const actions = (sent: TerminalCommand[]) => sent.map((command) => command.action);

test("a sign-in opens at its size, shows what the login prints, sends what is typed and ends", () => {
  const { session, sent, written, states, receive } = terminal();

  session.start();
  receive({ kind: "opened" });
  receive({ kind: "output", data: text("Paste the code") });
  session.input(new TextEncoder().encode("abc\r"));
  receive({ kind: "ended", sign_in: "signed_in" });
  receive({ kind: "closed" });

  assert.deepEqual(sent[0], { terminal: "t-1", machine: "m-1", workspace: "w-1", action: { kind: "open", agent: "claude", cols: 80, rows: 24 } });
  assert.deepEqual(actions(sent).slice(1), [{ kind: "input", data: text("abc\r") }]);
  assert.deepEqual(written, ["Paste the code"]);
  assert.deepEqual(states, [{ kind: "connecting" }, { kind: "open" }, { kind: "ended", signIn: "signed_in" }, { kind: "closed" }]);
});

test("a long paste goes in pieces, in order", () => {
  const { session, sent } = terminal();
  session.start();

  session.input(new Uint8Array(40_000).fill(0x61));

  const pieces = actions(sent)
    .slice(1)
    .map((action) => (action.kind === "input" ? action.data.length / 2 : -1));
  assert.deepEqual(pieces, [16_384, 16_384, 7_232]);
});

test("updates for another terminal or workspace, and anything after the close, change nothing", () => {
  const { session, written, states, receive } = terminal();
  session.start();

  session.receive({ terminal: "t-2", workspace: "w-1", event: { kind: "output", data: text("other") } });
  session.receive({ terminal: "t-1", workspace: "w-2", event: { kind: "closed" } });
  receive({ kind: "closed", code: "terminal.busy" });
  receive({ kind: "output", data: text("late") });
  session.input(new TextEncoder().encode("late"));

  assert.deepEqual(written, []);
  assert.deepEqual(states, [{ kind: "connecting" }, { kind: "closed", problem: "terminal.busy" }]);
});

test("closing asks the core to stop once, and only a terminal that was opened", () => {
  const unopened = terminal();
  unopened.session.close();
  unopened.session.start();
  const { session, sent, states } = terminal();
  session.start();
  session.start();

  session.close();
  session.close();

  assert.deepEqual(unopened.sent, []);
  assert.deepEqual(actions(sent), [{ kind: "open", agent: "claude", cols: 80, rows: 24 }, { kind: "close" }]);
  assert.deepEqual(states, [{ kind: "connecting" }, { kind: "closed" }]);
});

test("output that isn't bytes closes the terminal as broken and stops the core", () => {
  const { session, sent, written, states, receive } = terminal();
  session.start();

  receive({ kind: "output", data: "zz" });

  assert.deepEqual(written, []);
  assert.deepEqual(actions(sent).at(-1), { kind: "close" });
  assert.deepEqual(states.at(-1), { kind: "closed", problem: "broken" });
});

test("a connection that can't take a command, or is lost, closes the terminal as offline", () => {
  const unsent = terminal(false);
  unsent.session.start();
  const { session, sent, states } = terminal();
  session.start();

  session.lost();

  assert.deepEqual(unsent.states, [{ kind: "connecting" }, { kind: "closed", problem: "offline" }]);
  assert.deepEqual(states, [{ kind: "connecting" }, { kind: "closed", problem: "offline" }]);
  assert.equal(sent.length, 1);
});
