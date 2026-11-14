import "@xterm/xterm/css/xterm.css";
import { Terminal } from "@xterm/xterm";
import { useEffect, useRef, useState } from "react";

import type { Pins } from "../machines/pins.ts";
import type { AgentKind, MachineSummary, TerminalCommand, TerminalUpdate, WorkspaceId } from "../protocol.ts";
import { SignInTerminal, type TerminalState, closedOnce } from "./sign-in-terminal.ts";

const SIZE = { cols: 80, rows: 24 };

export type TerminalProps = {
  machine: MachineSummary;
  workspace: WorkspaceId;
  agent: AgentKind;
  pins: Pins;
  /** `false` when the connection can't take it, which ends the terminal. */
  send: (command: TerminalCommand) => boolean;
  /** Hands each terminal update to `handle` until the returned function is called. */
  subscribe: (handle: (update: TerminalUpdate) => void) => () => void;
  /** Called once the terminal has closed, whatever closed it. */
  onClosed?: () => void;
};

/**
 * A vendor's sign-in on the person's machine, sealed between this page and the machine. A new
 * machine key, workspace, agent or pins opens a new terminal; new callbacks don't.
 */
export function TerminalView(props: TerminalProps) {
  const { machine, workspace, agent, pins } = props;
  const latest = useRef(props);
  latest.current = props;
  const screen = useRef<HTMLDivElement>(null);
  const [state, setState] = useState<TerminalState>({ kind: "checking" });
  useEffect(() => {
    const host = screen.current;
    if (host === null) {
      return;
    }
    let mounted = true;
    const terminal = new Terminal({ ...SIZE, cursorBlink: true });
    terminal.open(host);
    const opened = new SignInTerminal({
      machine,
      workspace,
      agent,
      pins,
      send: (command) => latest.current.send(command),
      size: SIZE,
      screen: {
        write: (data) => terminal.write(data),
        state: closedOnce(setState, () => mounted && latest.current.onClosed?.()),
      },
    });
    const encoder = new TextEncoder();
    const typing = terminal.onData((data) => void opened.input(encoder.encode(data)));
    const unsubscribe = latest.current.subscribe((update) => void opened.receive(update));
    void opened.start();
    return () => {
      mounted = false;
      unsubscribe();
      typing.dispose();
      opened.close();
      terminal.dispose();
    };
  }, [machine.id, machine.key, workspace, agent, pins]);
  return (
    <div className="sign-in-terminal">
      {noteOf(state) !== undefined && <p className="muted">{noteOf(state)}</p>}
      <div ref={screen} hidden={state.kind === "checking"} />
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
        case "unpinned":
          return "Confirm this machine's key in Settings › Machines first.";
        case "offline":
          return "The connection to the server dropped. Try again once it's back.";
        case "key_changed":
          return "This machine's key changed. Pair it again.";
        case "key_unchecked":
          return "This machine's key couldn't be checked, so the terminal didn't open.";
        case "broken":
          return "The terminal closed because something didn't check out.";
        default:
          return "The machine closed the terminal.";
      }
  }
}
