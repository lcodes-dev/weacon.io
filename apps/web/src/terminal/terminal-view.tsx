import "@xterm/xterm/css/xterm.css";
import { Terminal } from "@xterm/xterm";
import { useEffect, useRef, useState } from "react";

import type { AgentKind, MachineSummary, TerminalCommand, TerminalUpdate, WorkspaceId } from "../protocol.ts";
import { SignInTerminal, type TerminalState } from "./sign-in-terminal.ts";
import { adoptStyles } from "./styles.ts";

const SIZE = { cols: 80, rows: 24 };

export type TerminalProps = {
  machine: MachineSummary;
  workspace: WorkspaceId;
  agent: AgentKind;
  /** `false` when the connection can't take it, which ends the terminal. */
  send: (command: TerminalCommand) => boolean;
  /** Hands each terminal update to `handle` until the returned function is called. */
  subscribe: (handle: (update: TerminalUpdate) => void) => () => void;
  /** Calls `handle` once the connection is gone, until the returned function is called. */
  lost: (handle: () => void) => () => void;
  /** Called once the terminal has closed, whatever closed it. */
  onClosed?: () => void;
};

/**
 * A vendor's sign-in on the person's machine. A new machine, workspace or agent opens a new
 * terminal; new callbacks don't.
 */
export function TerminalView(props: TerminalProps) {
  const { machine, workspace, agent } = props;
  const latest = useRef(props);
  latest.current = props;
  const screen = useRef<HTMLDivElement>(null);
  const [state, setState] = useState<TerminalState>({ kind: "connecting" });
  useEffect(() => {
    const host = screen.current;
    if (host === null) {
      return;
    }
    let mounted = true;
    const terminal = new Terminal({ ...SIZE, cursorBlink: true });
    const unstyle = adoptStyles(host);
    terminal.open(host);
    const opened = new SignInTerminal({
      machine,
      workspace,
      agent,
      send: (command) => latest.current.send(command),
      size: SIZE,
      screen: {
        write: (data) => terminal.write(data),
        state: (next) => {
          setState(next);
          if (next.kind === "closed" && mounted) {
            latest.current.onClosed?.();
          }
        },
      },
    });
    const encoder = new TextEncoder();
    const typing = terminal.onData((data) => opened.input(encoder.encode(data)));
    const unsubscribe = latest.current.subscribe((update) => opened.receive(update));
    const unwatch = latest.current.lost(() => opened.lost());
    opened.start();
    return () => {
      mounted = false;
      unsubscribe();
      unwatch();
      typing.dispose();
      opened.close();
      terminal.dispose();
      unstyle();
    };
  }, [machine.id, workspace, agent]);
  const note = noteOf(state);
  return (
    <div className="sign-in-terminal">
      {note !== undefined && <p className="muted">{note}</p>}
      <div ref={screen} />
    </div>
  );
}

function noteOf(state: TerminalState): string | undefined {
  switch (state.kind) {
    case "connecting":
      return "Connecting to the machine.";
    case "open":
      return undefined;
    case "closed":
      switch (state.problem) {
        case undefined:
          return "The terminal closed.";
        case "offline":
          return "The connection to the server dropped. Try again once it's back.";
        case "broken":
          return "The terminal closed because what the machine sent couldn't be read.";
        case "machine.offline":
          return "The machine isn't connected, or its core can't open sign-in terminals.";
        case "terminal.busy":
          return "A sign-in for this agent is already running on this machine.";
        default:
          return "The machine closed the terminal.";
      }
  }
}
