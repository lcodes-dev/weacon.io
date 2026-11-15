# Weacon

Weacon is a web harness for coding agents, with tasks, docs and team chats built in. It is open source, under the MIT licence.

## What it does

You and your team work with your coding agents in one place. Each person's agents run on their own Mac, PC or server, with their own sign-ins. The team shares the chats, tasks and docs.

With Weacon you can:

- chat with Claude Code, Codex and other agents from a browser, and follow what they do;
- keep tasks and docs next to the chats, and search them all with ⌘K (Ctrl+K on Linux and Windows);
- accept or reject the changes agents propose to docs and tasks;
- pair your machines and choose what each workspace's agents may do on them;
- sign your agents in from the web app, in a terminal sealed end to end, or with the vendor's own command line;
- ask the code map where things are in a repository, from the command line or from an agent.

## How it fits together

```text
  browser ──────────── HTTPS ──────────┐
  (the web app)                        ▼
                                 weacon-server ── SQLite
                                 (self-hosted or hosted; every workspace)
                                       ▲
  your Mac, PC or VPS                  │ dials out over TLS
  ┌────────────────────────────────────┴──┐
  │ weacon (the core) ── Claude Code,      │
  │   ▲                   Codex, ...       │
  │   │ native messaging                   │
  │ Chrome extension                       │
  └────────────────────────────────────────┘
```

- **The server** (`weacon-server`) holds every workspace in one SQLite database and serves the web app. You run your own or use a hosted one.
- **The core** (`weacon`) runs on each person's machine. It dials out to the server and drives that person's own Claude Code, Codex and other agents. It also serves a code map and a local MCP server to the agents it starts.
- **The Chrome extension** talks to the core on the same machine.
- **Any agent** will be able to use Weacon through the server's MCP endpoint.

## What Weacon promises

- **Your agent sign-ins never touch Weacon.** Agents run as the vendors' own programs. You sign in with the vendor's own tool. Weacon only reads whether you are signed in, and never stores, logs or forwards a credential.
- **Workspaces never mix.** An agent only runs with a sign-in from its own workspace. Nothing one workspace sees reveals another.
- **Nothing listens publicly but the server.** The core only dials out, over TLS it verifies. What it serves to its own agents listens on 127.0.0.1 only.
- **Agents propose, people decide.** Changes that agents make to docs and tasks arrive as proposals that a person accepts.
- **No content in logs.** Messages, prompts, code and credentials never reach a log line or an error report.

## Status

Weacon is in early development. This is what works on `main` today, checked by walking through the steps below against a real server and a real core:

| Area | Works today | Not yet |
| --- | --- | --- |
| Accounts | Email sign-in with a code; the first account owns the server | Sending the codes by email: they wait in a folder for a mail relay |
| Workspaces | A personal workspace for each account; new workspaces; members the operator adds with `weacon-server add-member` | Inviting people from the web app |
| Machines | Pairing with a key fingerprint check; online status; per-workspace agent access; error report choice | |
| Agent sign-in | The sealed sign-in terminal in a machine's settings; `weacon login` in a terminal | |
| Chats | The chat list and chat screen show chats the server has | Starting a chat from the web app: there is no button for it, and the server doesn't yet relay a prompt to your machine or keep its chat events. The list stays empty and says "No chats yet. Connect a machine to start one.", even with a machine connected |
| Tasks and docs | Adding, editing, statuses, doc revisions | |
| Proposals | Listing, accepting and rejecting | Nothing makes proposals yet: they will come from agents through the core |
| Search | ⌘K finds docs and tasks | |
| Code map | `weacon map` and the local MCP the core gives its agents | |
| Server MCP | | An MCP endpoint for agents Weacon didn't start |
| Chrome extension | Builds and registers with the core | |
| Updates | `weacon update schedule` | Installing updates: there is no signed release manifest yet |

## Run it on your machine

These steps run the server, a TLS front for it and the core on one Mac or Linux machine, for development and testing. Every piece keeps its data in the `.dev/` folder of your clone.

### 1. Install the tools

On macOS:

```bash
xcode-select --install
```

Install Rust with rustup from [rustup.rs](https://rustup.rs). The project pins its Rust version in `rust-toolchain.toml`, and rustup picks it up by itself.

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Install the Node version in `.node-version`, 22.23.4, with the version manager you prefer. With [fnm](https://github.com/Schniz/fnm):

```bash
brew install fnm
fnm install 22.23.4
fnm use 22.23.4
```

On Linux (Debian or Ubuntu), install git, a C compiler and curl, then rustup and Node as above:

```bash
sudo apt-get install -y git build-essential curl
```

To run an agent, install it and sign in to it yourself, with the vendor's own instructions. Weacon never installs or signs in an agent for you without asking.

### 2. Get the code and set it up

Clone into a short path, such as `~/code/weacon.io`: the core's socket lives under the clone, and macOS limits a socket's path to 103 bytes.

```bash
git clone https://github.com/lcodes-dev/weacon.io.git ~/code/weacon.io
cd ~/code/weacon.io
```

```bash
scripts/setup.sh
```

The first run takes a while: it fetches the dependencies, builds the check tools and installs the web app's and the extension's packages. Its log is in `.dev/setup.log`. It is safe to run again.

### 3. Start Weacon

Give your own email the first time. The first account on a server is its owner.

```bash
scripts/dev.sh you@example.com
```

It builds the web app, the server, the core and a TLS front, then starts them and prints:

```text
Weacon is running. Ctrl-C stops it.
  Web app:        http://localhost:8080
  Sign-in codes:  .dev/server/outbox, one JSON file per code
  Pair the core:  WEACON_STATE_DIR=.dev/state target/debug/weacon pair https://localhost:8443 --pin .dev/tls/cert.pem
  Logs:           .dev/server.log, .dev/tls.log, .dev/core.log and .dev/state/logs/
```

Leave it running, and open a second terminal in the same folder for the next steps. Later runs need no email: `scripts/dev.sh`.

What runs:

- the server on `http://localhost:8080`, with its database, sign-in codes and key in `.dev/server/`;
- `dev-tls`, a TLS front on `https://localhost:8443`, because the core only dials a server over TLS it can verify. It makes a certificate for `localhost` once, in `.dev/tls/`, and the core pins it. It is for development only;
- the core's daemon, with its state in `.dev/state/`.

### 4. Sign in

Open <http://localhost:8080> in Chrome, enter your email and choose **Send code**. The server writes the code to a file in `.dev/server/outbox` instead of sending an email. Read the newest one:

```bash
cat "$(ls -t .dev/server/outbox/*.json | head -n 1)"
```

Enter its `code` and choose **Sign in**. You see **Workspaces**, with your **Personal** workspace. Make more under **New workspace**.

### 5. Pair the core

In the second terminal:

```bash
export WEACON_STATE_DIR=.dev/state
target/debug/weacon pair https://localhost:8443 --pin .dev/tls/cert.pem --name "Dev laptop"
```

It prints a code and a key fingerprint, then waits. In the web app, open **Settings**, enter the code under **Pair a machine** and choose **Look up**. Check that the fingerprint matches the terminal's, then choose **Confirm**. The terminal says `Paired.`

Within 30 seconds the daemon connects, and the machine's row shows `weacon 0.0.0`. Check from the terminal:

```bash
target/debug/weacon doctor
```

### 6. Set up the machine for a workspace

Open your workspace, then **Settings**, then the machine's **Settings**. There you choose:

- **Error reports**: off until you turn them on.
- **Agent access**: what the workspace's agents may do on this machine: off, read only, ask first or full.
- **Sign-ins**: each agent's state. **Sign in** opens the vendor's own sign-in in a terminal sealed between the browser and your core. The server only relays bytes it can't read.

To sign in from a terminal instead, use the workspace's id, the `workspace=` part of the web app's address:

```bash
target/debug/weacon login claude --workspace <workspace id>
```

Give the workspace a repository its chats may work in, by its absolute path:

```bash
target/debug/weacon repo add --workspace <workspace id> demo "$HOME/code/demo"
target/debug/weacon repo list --workspace <workspace id>
```

### 7. Try the rest

- **Tasks**: type a title under **New task** and choose **Add**. Change its status from the list.
- **Docs**: **New doc**, then a title and text, then **Save**. Each edit is a new revision.
- **Proposals**: changes agents propose wait here. Nothing makes them yet.
- **Search**: press ⌘K (Ctrl+K on Linux), then type.
- **Chats**: the list is empty, because nothing starts a chat yet; see Status.
- **Teammates**: the operator adds them from the command line. Each signs in with their own email and pairs their own machine. With the server running:

```bash
target/debug/weacon-server create-workspace --db .dev/server/weacon.db --name "Team"
target/debug/weacon-server add-member --db .dev/server/weacon.db --workspace <workspace id> --email grace@example.com
target/debug/weacon-server members --db .dev/server/weacon.db --workspace <workspace id>
```

`create-workspace` prints the new workspace's id. Add yourself too, to see it in the web app.

- **Code map**: what the core's agents get through its local MCP. From the root of any repository:

```bash
target/debug/weacon map
target/debug/weacon map search pairing code
target/debug/weacon map hotspots
```

### Stop, data and starting over

Ctrl-C in the first terminal stops the server, the TLS front and the core. `scripts/dev.sh` starts them again, with your account, workspaces and pairing as they were.

| Folder | What it holds |
| --- | --- |
| `.dev/server/` | The server's database, its key for sign-in codes, and the codes in `outbox/` |
| `.dev/tls/` | The TLS front's certificate and key |
| `.dev/state/` | The core's state: its pairing, settings, sign-in folders and logs |
| `.dev/work/` | The folders agents work in |

To start over, stop Weacon, then remove the server's and the core's data together, since a core paired with a server you removed can't connect:

```bash
rm -rf .dev/server .dev/state .dev/work
scripts/dev.sh you@example.com
```

## Development

### Tests and checks

Everything CI runs, in order. Run it before every commit:

```bash
scripts/check.sh
```

Everything but the mutation tests, which take longest:

```bash
scripts/check.sh --mutants none
```

One crate's tests, or the ones whose names match:

```bash
cargo test --locked -p weacon-server
cargo test --locked -p weacon-core pair
```

The web app's type check, build, unit tests and end-to-end tests in Chromium:

```bash
npm --prefix apps/web run check
```

Format and apply the lint fixes clippy is sure of:

```bash
scripts/fix.sh
```

### The Chrome extension

```bash
npm --prefix apps/extension run build
```

In Chrome, open `chrome://extensions`, turn on **Developer mode**, choose **Load unpacked** and pick `apps/extension/dist`. Then let it reach the core:

```bash
target/debug/weacon connect-chrome
```

Chrome starts the core's native host without your shell's variables, so it uses the core's default state folder, not `.dev/state`.

### The layout

| Folder | What it is |
| --- | --- |
| `apps/server` | `weacon-server`: the API, the web app's files and the operator's commands |
| `apps/core` | `weacon`: the core, its daemon and its command line |
| `apps/web` | The web app, in TypeScript and React, built into the server |
| `apps/extension` | The Chrome extension |
| `crates/protocol` | Every message between the programs, and its schema |
| `crates/agents` | Starts and talks to agents |
| `crates/client`, `crates/model` | What the core and server share about the server's API and its data |
| `crates/codemap` | The code map |
| `crates/observability`, `crates/appkit`, `crates/version` | Logs, error reports, app startup and build versions |
| `crates/testing` | Fakes and test helpers, `fake-server` and `dev-tls` |
| `integrations/` | The plugin files, hooks and skills Weacon gives each vendor's agents |
| `scripts/` | Setup, checks, the dev launcher and releases |
| `tooling/` | The architecture test and the shared TypeScript tooling |

### Contributing

Start with [`AGENTS.md`](AGENTS.md): the rules every change keeps, the code style and the definition of done. Coding agents read it too. Every change reaches `main` through a pull request.

## Troubleshooting

- **The daemon doesn't start, with `daemon.start_failed`.** The clone's path is too long for the core's socket. Clone into a shorter path; `scripts/dev.sh` checks this first.
- **`scripts/setup.sh` says Weacon needs another Node.** Install the version in `.node-version` and make it the one on your `PATH`, then run it again.
- **`scripts/dev.sh` says the server didn't start.** Something else uses port 8080, or 8443 for the TLS front. Stop it, then run again. Read `.dev/server.log` and `.dev/tls.log`.
- **`weacon-server add-account` says there is no database.** The server makes the database when it first starts; `scripts/dev.sh` adds the account after that.
- **The machine shows no version after pairing.** The daemon looks for its pairing every 30 seconds. Check that `scripts/dev.sh` is still running, then read `.dev/state/logs/daemon.log`.
- **`update check failed` in the core's log.** There is no release to check against yet. Turn the checks off with `target/debug/weacon update schedule off`.
- **The sign-in page never accepts the code.** Use the newest file in `.dev/server/outbox`, and request a new code if it expired.

## Licence

MIT. See [`LICENSE`](LICENSE).
