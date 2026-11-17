import { hex, unhex } from "../hex.ts";
import { newId } from "../ids.ts";
import type { AgentKind, Code, MachineSummary, TerminalAction, TerminalCommand, TerminalId, TerminalUpdate, WorkspaceId } from "../protocol.ts";

/** Where a sign-in terminal stands, for the view to say. */
export type TerminalState = { kind: "connecting" } | { kind: "open" } | { kind: "closed"; problem?: Problem };

/** Why a terminal closed before its sign-in ended. */
export type Problem = "offline" | "broken" | Code;

/** What a terminal shows. Output goes nowhere but `write`; a terminal is closed once at most. */
export interface Screen {
  write(data: Uint8Array): void;
  state(state: TerminalState): void;
}

export type Size = { cols: number; rows: number };

export type Options = {
  machine: MachineSummary;
  workspace: WorkspaceId;
  agent: AgentKind;
  /** Puts a command on the server's connection; `false` when it can't, which ends the terminal. */
  send: (command: TerminalCommand) => boolean;
  screen: Screen;
  size: Size;
  terminal?: TerminalId;
};

/** The browser's end of a sign-in terminal, which closes for good at the first thing that fails. */
export class SignInTerminal {
  readonly #options: Options;
  readonly #terminal: TerminalId;
  #done = false;
  #started = false;

  constructor(options: Options) {
    this.#options = options;
    this.#terminal = options.terminal ?? newId();
  }

  /** Opens the terminal; once only, and not once closed. */
  start(): void {
    if (this.#done || this.#started) {
      return;
    }
    this.#started = true;
    this.#options.screen.state({ kind: "connecting" });
    this.#send({ kind: "open", agent: this.#options.agent, ...this.#options.size });
  }

  /** Takes one update about this terminal. */
  receive(update: TerminalUpdate): void {
    if (this.#done || update.terminal !== this.#terminal || update.workspace !== this.#options.workspace) {
      return;
    }
    const { screen } = this.#options;
    const event = update.event;
    switch (event.kind) {
      case "opened":
        return screen.state({ kind: "open" });
      case "output": {
        const data = bytesOf(event.data);
        return data === undefined ? this.#stop("broken") : screen.write(data);
      }
      case "closed":
        return this.#close(event.code);
    }
  }

  /** Sends what was typed or pasted. */
  input(data: Uint8Array): void {
    this.#act({ kind: "input", data: hex(data) });
  }

  /** Ends the sign-in, if it still runs. */
  close(): void {
    this.#stop();
  }

  /** Closes the terminal once its connection has gone, which ends it on the machine too. */
  lost(): void {
    this.#close("offline");
  }

  #act(action: TerminalAction): void {
    if (this.#started && !this.#done) {
      this.#send(action);
    }
  }

  #send(action: TerminalAction): void {
    const { machine, workspace, send } = this.#options;
    if (!send({ terminal: this.#terminal, machine: machine.id, workspace, action })) {
      this.#close("offline");
    }
  }

  // Asks the core to stop, if it has heard of this terminal.
  #stop(problem?: Problem): void {
    this.#act({ kind: "close" });
    this.#close(problem);
  }

  #close(problem?: Problem): void {
    if (this.#done) {
      return;
    }
    this.#done = true;
    this.#options.screen.state({ kind: "closed", ...(problem === undefined ? {} : { problem }) });
  }
}

// The bytes `text` spells, if it is lowercase hex.
function bytesOf(text: string): Uint8Array<ArrayBuffer> | undefined {
  return /^(?:[0-9a-f]{2})*$/.test(text) ? unhex(text) : undefined;
}
