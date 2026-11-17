// The real core's daemon, paired through the page with the real server, and a
// stand-in that replays recorded Claude Code sessions in place of the agent.

import { type ChildProcess, execFile, spawn } from "node:child_process";
import { once } from "node:events";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, symlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, join } from "node:path";
import { promisify } from "node:util";

import { REPO, firstLine, waitFor } from "./harness.ts";
import { confirm } from "./pairing.ts";
import type { Served } from "./server.ts";

const run = promisify(execFile);

/** A machine whose core works for one workspace, in a repository of its own. */
export type Machine = {
  /** Starts the core's daemon, and waits until it is connected to the server. */
  start(): Promise<void>;
  /** Stops the daemon and waits for it to exit. */
  stop(): Promise<void>;
  /** The prompts its stand-in agent was given, in order, as the agent read them. */
  heard(): string[];
  /** Stops everything this started and removes its folder. */
  close(): Promise<void>;
};

/** Builds the core and the stand-in agent, and gives where each program is. */
async function build(): Promise<{ core: string; agent: string }> {
  // Apart, as scripts/dev.sh builds them, so neither changes the other's features.
  const core = await executables("-p", "weacon-core", "--bin", "weacon");
  const testing = await executables("-p", "weacon-testing", "--bin", "fake-agent");
  const at = (built: Map<string, string>, name: string) => built.get(name) ?? fail(`cargo built no ${name}`);
  return { core: at(core, "weacon"), agent: at(testing, "fake-agent") };
}

/** Builds what `args` select, and gives each program's path by its name. */
async function executables(...args: string[]): Promise<Map<string, string>> {
  const { stdout } = await run("cargo", ["build", "--locked", "--quiet", "--message-format=json", ...args], { cwd: REPO, maxBuffer: 1 << 28 });
  const built = new Map<string, string>();
  for (const line of stdout.split("\n")) {
    const message = JSON.parse(line || "{}") as { reason?: string; target?: { name: string }; executable?: string | null };
    if (message.reason === "compiler-artifact" && message.target !== undefined && typeof message.executable === "string") {
      built.set(message.target.name, message.executable);
    }
  }
  return built;
}

function fail(why: string): never {
  throw new Error(why);
}

/**
 * Pairs a core named `name` through the page, which must be on Settings of `workspace`, gives the
 * workspace a repository `demo` on it and starts its daemon; on any failure, stops what it started.
 */
export async function machine(served: Served, workspace: string, name: string): Promise<Machine> {
  const dir = mkdtempSync(join(tmpdir(), "weacon-core-"));
  const children: ChildProcess[] = [];
  const close = async () => {
    await Promise.all(children.map(end));
    rmSync(dir, { recursive: true, force: true });
  };
  try {
    return { ...(await setUp(served, workspace, name, dir, children)), close };
  } catch (error) {
    await close();
    throw error;
  }
}

async function setUp(served: Served, workspace: string, name: string, dir: string, children: ChildProcess[]): Promise<Omit<Machine, "close">> {
  const programs = await build();
  const bin = join(dir, "bin");
  mkdirSync(bin);
  // Ahead of any installed `claude`, for the core's runs only.
  symlinkSync(programs.agent, join(bin, "claude"));
  const home = join(dir, "home");
  mkdirSync(home);
  const heard = join(dir, "heard.jsonl");
  const env = { ...process.env, HOME: home, WEACON_STATE_DIR: join(dir, "state"), FAKE_AGENT_HEARD: heard, PATH: `${bin}${delimiter}${process.env.PATH ?? ""}` };
  const started = (child: ChildProcess) => {
    children.push(child);
    return child;
  };

  const address = `http://127.0.0.1:${new URL(served.page.url()).port}`;
  const pairing = started(spawn(programs.core, ["pair", address, "--name", name], { env, stdio: ["ignore", "pipe", "inherit"] }));
  const code = await firstLine(pairing, 10_000, (line) => /pair this machine: (\S+)$/.exec(line)?.[1]);
  await confirm(served.page, code);
  // It claims the pairing every 2 seconds.
  await waitFor(() => pairing.exitCode !== null, 30_000);
  if (pairing.exitCode !== 0) {
    throw new Error("the core didn't pair");
  }

  const repo = join(dir, "repo");
  // Without the person's own git settings, such as signing every commit.
  const git = { env: { ...env, GIT_CONFIG_GLOBAL: join(dir, "gitconfig"), GIT_CONFIG_NOSYSTEM: "1" } };
  await run("git", ["init", "--quiet", repo], git);
  await run("git", ["-C", repo, "-c", "user.name=Ada", "-c", "user.email=ada@example.com", "commit", "--quiet", "--allow-empty", "-m", "Start"], git);
  await run(programs.core, ["repo", "add", "--workspace", workspace, "demo", repo], { env });

  let daemon: ChildProcess | undefined;
  const log = join(dir, "state", "logs", "daemon.log");
  const connections = () => (existsSync(log) ? readFileSync(log, "utf8").split("\n").filter((line) => line.includes('"msg":"uplink connected"')).length : 0);
  const machine = {
    heard: () =>
      (existsSync(heard) ? readFileSync(heard, "utf8") : "")
        .split("\n")
        .filter((line) => line !== "")
        .map((line) => JSON.parse(line) as string),
    start: async () => {
      const before = connections();
      const child = started(spawn(programs.core, ["daemon"], { env, stdio: "ignore" }));
      daemon = child;
      await waitFor(() => connections() > before || child.exitCode !== null);
      if (child.exitCode !== null) {
        throw new Error("the core's daemon stopped");
      }
    },
    stop: async () => {
      if (daemon !== undefined) {
        await end(daemon);
      }
    },
  };
  await machine.start();
  return machine;
}

/** Stops `child` and waits for it to exit, unless it already has. */
async function end(child: ChildProcess): Promise<void> {
  if (child.exitCode !== null || child.signalCode !== null) {
    return;
  }
  const exited = once(child, "exit");
  child.kill();
  await exited;
}
