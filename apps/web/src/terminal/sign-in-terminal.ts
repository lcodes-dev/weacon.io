import type { Pins } from "../machines/pins.ts";
import { hex, unhex } from "../hex.ts";
import { newId } from "../ids.ts";
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
  | { kind: "connecting" }
  | { kind: "open" }
  | { kind: "ended"; signIn: SignInState }
  | { kind: "closed"; problem?: Problem };

/**
 * Why a terminal closed before its sign-in ended. `unpinned`: this browser hasn't confirmed the
 * machine's key, which only the pairing confirm in Settings does.
 */
export type Problem = "unpinned" | "key_changed" | "key_unchecked" | "offline" | "broken" | Code;

/** What a terminal shows. Output goes nowhere but `write`; a terminal is closed once at most. */
export interface Screen {
  write(data: Uint8Array): void;
  state(state: TerminalState): void;
}

export type Size = { cols: number; rows: number };

// Twice this as hex, in its JSON, stays within one sealed frame.
const INPUT_PIECE = 16_384;

// How often an open terminal tells the core it is still there; the core closes one quiet for a minute.
const BEAT_MS = 20_000;

/** Calls `run` every `ms` until the returned function is called. */
export type Every = (run: () => void, ms: number) => () => void;

const every: Every = (run, ms) => {
  const timer = setInterval(run, ms);
  return () => clearInterval(timer);
};

export type Options = {
  machine: MachineSummary;
  workspace: WorkspaceId;
  agent: AgentKind;
  pins: Pins;
  /** Puts a command on the server's connection; `false` when it can't, which ends the terminal. */
  send: (command: TerminalCommand) => boolean;
  screen: Screen;
  size: Size;
  terminal?: TerminalId;
  ephemeral?: Promise<Ephemeral>;
  every?: Every;
};

/**
 * The browser's end of a sign-in terminal. It seals only to the key this browser pinned for the
 * machine at pairing, never pins one itself, and closes for good at the first thing that doesn't
 * check out.
 */
export class SignInTerminal {
  readonly #options: Options;
  readonly #terminal: TerminalId;
  #pending: Pending | undefined;
  #channel: Channel | undefined;
  #done = false;
  #started = false;
  // Whether the core has heard of this terminal, and so has one to close.
  #sent = false;
  #received: Promise<void> = Promise.resolve();
  #typed: Promise<void> = Promise.resolve();
  #stopBeating = () => {};

  constructor(options: Options) {
    this.#options = options;
    this.#terminal = options.terminal ?? newId();
  }

  /**
   * Checks the machine's key against this browser's pin, then opens; once only, and not once
   * closed, as a handshake nobody finishes would hold the machine's slot.
   */
  async start(): Promise<void> {
    if (this.#done || this.#started) {
      return;
    }
    this.#started = true;
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
    if (check !== "match") {
      return this.#close(check === "mismatch" ? "key_changed" : "unpinned");
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

  /**
   * Sends what was typed, after whatever was typed before it; a long paste goes in pieces that
   * each fit one sealed frame.
   */
  input(data: Uint8Array): Promise<void> {
    this.#typed = this.#typed.then(async () => {
      for (let at = 0; at < data.length; at += INPUT_PIECE) {
        await this.#seal({ kind: "input", data: hex(data.subarray(at, at + INPUT_PIECE)) });
      }
    });
    return this.#typed;
  }

  resize(size: Size): Promise<void> {
    return this.#seal({ kind: "resize", ...size });
  }

  /** Ends the sign-in, if it still runs. */
  close(): void {
    this.#stop();
  }

  /** Closes the terminal once its connection has gone, which ends it on the machine too. */
  lost(): void {
    this.#close("offline");
  }

  async #open(): Promise<void> {
    const { machine, size, screen } = this.#options;
    const remote = bytesOf(machine.key);
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
      this.#sent = true;
      this.#send({ kind: "open", agent: this.#options.agent, handshake: hex(message) });
    } catch {
      this.#fail();
    }
  }

  async #accept(handshake: string): Promise<void> {
    const pending = this.#pending;
    const reply = bytesOf(handshake);
    this.#pending = undefined;
    if (pending === undefined || reply === undefined) {
      throw new Error("no handshake to finish");
    }
    const { channel, payload } = await pending.finish(reply);
    if (this.#done) {
      return channel.close();
    }
    if (payload.length > 0) {
      throw new Error("a reply with a payload");
    }
    this.#channel = channel;
    this.#options.screen.state({ kind: "open" });
    // The same size again changes nothing on the machine.
    this.#stopBeating = (this.#options.every ?? every)(() => void this.resize(this.#options.size), BEAT_MS);
    // The core starts the login only once a frame shows this end holds the keys.
    await this.resize(this.#options.size);
  }

  async #opened(sealed: string): Promise<Shown | undefined> {
    const message = bytesOf(sealed);
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
    if (!send({ terminal: this.#terminal, machine: machine.id, workspace, action })) {
      this.#close("offline");
    }
  }

  // Shows nothing more once anything fails, and asks the core to stop too.
  #fail(): void {
    this.#stop("broken");
  }

  #stop(problem?: Problem): void {
    if (!this.#done && this.#sent) {
      this.#send({ kind: "close" });
    }
    this.#close(problem);
  }

  #close(problem?: Problem): void {
    if (this.#done) {
      return;
    }
    this.#done = true;
    this.#stopBeating();
    this.#channel?.close();
    this.#options.screen.state({ kind: "closed", ...(problem === undefined ? {} : { problem }) });
  }
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
    const data = typeof output.data === "string" ? bytesOf(output.data) : undefined;
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

// The bytes `text` spells, if it is lowercase hex.
function bytesOf(text: string): Uint8Array<ArrayBuffer> | undefined {
  return /^(?:[0-9a-f]{2})*$/.test(text) ? unhex(text) : undefined;
}
