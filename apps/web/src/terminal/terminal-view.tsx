import "@xterm/xterm/css/xterm.css";
import { Terminal } from "@xterm/xterm";
import { useEffect, useRef, useState } from "react";

import type { Pins } from "../machines/pins.ts";
import type { AgentKind, MachineSummary, TerminalCommand, TerminalUpdate, WorkspaceId } from "../protocol.ts";
import { SignInTerminal, type TerminalState } from "./sign-in-terminal.ts";

const SIZE = { cols: 80, rows: 24 };

export type TerminalProps = {
  machine: MachineSummary;
  workspace: WorkspaceId;
  agent: AgentKind;
  pins: Pins;
  send: (command: TerminalCommand) => void;
  /** Hands each terminal update to `handle` until the returned function is called. */
  subscribe: (handle: (update: TerminalUpdate) => void) => () => void;
};

/** A vendor's sign-in on the person's machine, sealed between this page and the machine. */
export function TerminalView(props: TerminalProps) {
  const { machine, workspace, agent, pins, send, subscribe } = props;
  const screen = useRef<HTMLDivElement>(null);
  const session = useRef<SignInTerminal>(undefined);
  const [state, setState] = useState<TerminalState>({ kind: "checking" });
  useEffect(() => {
    const host = screen.current;
    if (host === null) {
      return;
    }
    const terminal = new Terminal({ ...SIZE, cursorBlink: true });
    terminal.open(host);
    const opened = new SignInTerminal({
      machine,
      workspace,
      agent,
      pins,
      send,
      size: SIZE,
      screen: { write: (data) => terminal.write(data), state: setState },
    });
    session.current = opened;
    const encoder = new TextEncoder();
    const typing = terminal.onData((data) => void opened.input(encoder.encode(data)));
    const unsubscribe = subscribe((update) => void opened.receive(update));
    void opened.start();
    return () => {
      unsubscribe();
      typing.dispose();
      opened.close();
      terminal.dispose();
    };
  }, [machine, workspace, agent, pins, send, subscribe]);
  return (
    <div className="sign-in-terminal">
      <TerminalNote state={state} confirm={() => void session.current?.confirm()} />
      <div ref={screen} hidden={state.kind === "confirm" || state.kind === "checking"} />
    </div>
  );
}

function TerminalNote({ state, confirm }: { state: TerminalState; confirm: () => void }) {
  if (state.kind !== "confirm") {
    const note = noteOf(state);
    return note === undefined ? null : <p className="muted">{note}</p>;
  }
  return (
    <div>
      <p>Check that this fingerprint matches the one Weacon shows on the machine.</p>
      <p>
        <code>{state.fingerprint}</code>
      </p>
      <button type="button" onClick={confirm}>
        It matches
      </button>
    </div>
  );
}

function noteOf(state: TerminalState): string | undefined {
  switch (state.kind) {
    case "checking":
      return "Checking the machine's key.";
    case "connecting":
      return "Connecting to the machine.";
    case "open":
      return undefined;
    case "ended":
      return state.signIn === "signed_in" ? "Signed in." : "The sign-in ended without signing in.";
    case "closed":
      switch (state.problem) {
        case undefined:
          return "The terminal closed.";
        case "key_changed":
          return "This machine's key changed. Pair it again.";
        case "key_unchecked":
          return "This machine's key couldn't be checked, so the terminal didn't open.";
        case "broken":
          return "The terminal closed because something didn't check out.";
        default:
          return "The machine closed the terminal.";
      }
    case "confirm":
      return undefined;
  }
}
