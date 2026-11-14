import { fingerprint } from "../machines/fingerprint.ts";
import type { Pins } from "../machines/pins.ts";
import { fromHex, hex } from "../hex.ts";
import type {
  AgentKind,
  Code,
  MachineSummary,
  SignInState,
  TerminalAction,
  TerminalCommand,
  TerminalId,
  TerminalInput,
  TerminalOutput,
  TerminalUpdate,
  WorkspaceId,
} from "../protocol.ts";
import { type Channel, type Ephemeral, type Pending, ephemeral, initiate } from "./noise.ts";

/** Where a sign-in terminal stands, for the view to say. */
export type TerminalState =
  | { kind: "checking" }
  /** No key is pinned for the machine: the person compares `fingerprint` with the machine's. */
  | { kind: "confirm"; fingerprint: string }
  | { kind: "connecting" }
  | { kind: "open" }
  | { kind: "ended"; signIn: SignInState }
  | { kind: "closed"; problem?: Problem };

/** Why a terminal closed before its sign-in ended. */
export type Problem = "key_changed" | "key_unchecked" | "broken" | Code;

/** What a terminal shows. Output goes nowhere but `write`. */
export interface Screen {
  write(data: Uint8Array): void;
  state(state: TerminalState): void;
}

export type Size = { cols: number; rows: number };

export type Options = {
  machine: MachineSummary;
  workspace: WorkspaceId;
  agent: AgentKind;
  pins: Pins;
  send: (command: TerminalCommand) => void;
  screen: Screen;
  size: Size;
  terminal?: TerminalId;
  ephemeral?: Promise<Ephemeral>;
};

/**
 * The browser's end of a sign-in terminal. It seals to the key this browser pinned for the
 * machine, never to one it hasn't, and closes for good at the first thing that doesn't check
 * out.
 */
export class SignInTerminal {
  readonly #options: Options;
  readonly #terminal: TerminalId;
  #pending: Pending | undefined;
  #channel: Channel | undefined;
  #done = false;
  #handshaken = false;
  #received: Promise<void> = Promise.resolve();

  constructor(options: Options) {
    this.#options = options;
    this.#terminal = options.terminal ?? hex(crypto.getRandomValues(new Uint8Array(16)));
  }

  /** Checks the machine's key against this browser's pin, then opens, or asks the person. */
  async start(): Promise<void> {
    const { machine, pins, screen } = this.#options;
    screen.state({ kind: "checking" });
    let check;
    try {
      check = await pins.matches(machine.id, machine.key);
    } catch {
      return this.#close("key_unchecked");
    }
    if (this.#done) {
      return;
    }
    if (check === "mismatch") {
      return this.#close("key_changed");
    }
    if (check === "unpinned") {
      const shown = await fingerprint(machine.key);
      return this.#done ? undefined : screen.state({ kind: "confirm", fingerprint: shown });
    }
    await this.#open();
  }

  /** The person said the fingerprint matches the machine's: pins its key, then opens. */
  async confirm(): Promise<void> {
    const { machine, pins } = this.#options;
    if (this.#done || this.#handshaken) {
      return;
    }
    try {
      await pins.pin(machine.id, machine.key);
    } catch {
      return this.#close("key_unchecked");
    }
    await this.#open();
  }

  /** Takes one update about this terminal; updates are handled one at a time, in the order they came. */
  receive(update: TerminalUpdate): Promise<void> {
    this.#received = this.#received.then(() => this.#receive(update));
    return this.#received;
  }

  async #receive(update: TerminalUpdate): Promise<void> {
    if (this.#done || update.terminal !== this.#terminal || update.workspace !== this.#options.workspace) {
      return;
    }
    const event = update.event;
    if (event.kind === "closed") {
      return this.#close(event.code);
    }
    try {
      if (event.kind === "accepted") {
        await this.#accept(event.handshake);
      } else {
        this.#show(await this.#opened(event.sealed));
      }
    } catch {
      this.#fail();
    }
  }

  input(data: Uint8Array): Promise<void> {
    return this.#seal({ kind: "input", data: hex(data) });
  }

  resize(size: Size): Promise<void> {
    return this.#seal({ kind: "resize", ...size });
  }

  /** Ends the sign-in, if it still runs. */
  close(): void {
    this.#end();
  }

  // Once only, and not once closed: a handshake nobody finishes would hold the machine's slot.
  async #open(): Promise<void> {
    if (this.#done || this.#handshaken) {
      return;
    }
    this.#handshaken = true;
    const { machine, size, screen } = this.#options;
    const remote = fromHex(machine.key);
    if (remote === undefined) {
      return this.#close("key_unchecked");
    }
    screen.state({ kind: "connecting" });
    const resize = json({ kind: "resize", ...size } satisfies TerminalInput);
    try {
      const { message, pending } = await initiate(remote, this.#prologue(), resize, this.#options.ephemeral ?? ephemeral());
      if (this.#done) {
        return;
      }
      this.#pending = pending;
      this.#send({ kind: "open", agent: this.#options.agent, handshake: hex(message) });
    } catch {
      this.#fail();
    }
  }

  async #accept(handshake: string): Promise<void> {
    const pending = this.#pending;
    const reply = fromHex(handshake);
    this.#pending = undefined;
    if (pending === undefined || reply === undefined) {
      throw new Error("no handshake to finish");
    }
    const { channel, payload } = await pending.finish(reply);
    if (payload.length > 0) {
      throw new Error("a reply with a payload");
    }
    this.#channel = channel;
    this.#options.screen.state({ kind: "open" });
    // The core starts the login only once a frame shows this end holds the keys.
    await this.resize(this.#options.size);
  }

  async #opened(sealed: string): Promise<Shown | undefined> {
    const message = fromHex(sealed);
    if (this.#channel === undefined || message === undefined) {
      throw new Error("no channel");
    }
    return outputOf(new TextDecoder("utf-8", { fatal: true }).decode(await this.#channel.open(message)));
  }

  #show(output: Shown | undefined): void {
    if (this.#done || output === undefined) {
      return;
    }
    if (output.kind === "output") {
      this.#options.screen.write(output.data);
    } else {
      this.#options.screen.state({ kind: "ended", signIn: output.sign_in });
    }
  }

  async #seal(input: TerminalInput): Promise<void> {
    const channel = this.#channel;
    if (this.#done || channel === undefined) {
      return;
    }
    try {
      this.#send({ kind: "frame", sealed: hex(await channel.seal(json(input))) });
    } catch {
      this.#fail();
    }
  }

  #prologue(): Uint8Array<ArrayBuffer> {
    const { machine, workspace, agent } = this.#options;
    return prologue(machine.id, workspace, this.#terminal, agent);
  }

  #send(action: TerminalAction): void {
    const { machine, workspace, send } = this.#options;
    send({ terminal: this.#terminal, machine: machine.id, workspace, action });
  }

  // Shows nothing more once anything fails, and asks the core to stop too.
  #fail(): void {
    this.#end("broken");
  }

  #end(problem?: Problem): void {
    if (!this.#done) {
      this.#send({ kind: "close" });
      this.#close(problem);
    }
  }

  #close(problem?: Problem): void {
    if (this.#done) {
      return;
    }
    this.#done = true;
    this.#channel?.close();
    this.#options.screen.state({ kind: "closed", ...(problem === undefined ? {} : { problem }) });
  }
}

/** Passes each state on, and calls `closed` the first time one is closed. */
export function closedOnce(state: (state: TerminalState) => void, closed: () => void): (next: TerminalState) => void {
  let called = false;
  return (next) => {
    state(next);
    if (next.kind === "closed" && !called) {
      called = true;
      closed();
    }
  };
}

/** What both ends hash before the handshake, as the core's `TerminalCommand::prologue`. */
export function prologue(machine: string, workspace: string, terminal: string, agent: AgentKind): Uint8Array<ArrayBuffer> {
  return new TextEncoder().encode(["weacon-terminal/1", machine, workspace, terminal, agent].join("\n"));
}

function json(input: TerminalInput): Uint8Array<ArrayBuffer> {
  return new TextEncoder().encode(JSON.stringify(input));
}

// What the core sent, with its bytes read.
type Shown = { kind: "output"; data: Uint8Array } | Extract<TerminalOutput, { kind: "ended" }>;

// The core's output, checked where it arrives: `undefined` for a kind from a newer core, and
// an error for anything else that isn't one.
function outputOf(text: string): Shown | undefined {
  const parsed: unknown = JSON.parse(text);
  if (typeof parsed !== "object" || parsed === null || !("kind" in parsed) || typeof parsed.kind !== "string") {
    throw new Error("not an output");
  }
  const output = parsed as Record<string, unknown>;
  if (output.kind === "output") {
    const data = typeof output.data === "string" ? fromHex(output.data) : undefined;
    if (data === undefined) {
      throw new Error("not an output");
    }
    return { kind: "output", data };
  }
  if (output.kind === "ended") {
    const known: readonly unknown[] = ["signed_in", "signed_out", "not_installed", "unknown"];
    return { kind: "ended", sign_in: known.includes(output.sign_in) ? (output.sign_in as SignInState) : "unknown" };
  }
  return undefined;
}
