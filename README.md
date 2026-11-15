# Weacon

Weacon is a web harness for coding agents, with tasks, docs and team chats built in.

## What it does

You and your team work with your coding agents in one place. Each person's agents run on their own Mac, PC or server, with their own sign-ins. The team shares the chats, tasks and docs.

With Weacon you can:

- chat with Claude Code, Codex and other agents from a browser, and follow what they do;
- keep tasks and docs next to the chats, and search them all with ⌘K (Ctrl+K on Linux and Windows);
- accept or reject the changes agents propose to docs and tasks;
- pair your machines and choose what each workspace's agents may do on them;
- sign your agents in from the web app, in a terminal sealed end to end, or with the vendor's own command line;
- ask the code map where things are in a repository, from the command line or from an agent.

Weacon is open source, under the MIT licence.

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

- **Your agent sign-ins never touch Weacon.** Agents run as the vendors' own programs. You sign in with the vendor's own tool. Weacon only reads whether you are signed in, and never stores, logs or forwards a credential. The sign-in terminal in the web app is sealed between your browser and your core, but it trusts whoever runs the server, which serves the web app's code; if you don't, sign in over SSH on the machine instead.
- **Workspaces never mix.** An agent only runs with a sign-in from its own workspace. Nothing one workspace sees reveals another.
- **Nothing listens publicly but the server.** The core only dials out, over TLS it verifies. What it serves to its own agents listens on 127.0.0.1 only.
- **Agents propose, people decide.** Changes that agents make to docs and tasks arrive as proposals that a person accepts.
- **No content in logs.** Messages, prompts, code and credentials never reach a log line or an error report. The one exception: until the server sends mail itself, it prints each mail it would send, sign-in codes included, to its own log.

## Status

Weacon is in early development. This is what works on `main` today:

| Area | Works today | Not yet |
| --- | --- | --- |
| Accounts | Email sign-in with a code; the first account owns the server | Sending the codes by email: nothing sends mail yet, so the server prints each one to its log |
| Workspaces | A personal workspace for each account; new workspaces; members the operator adds with `weacon-server add-member` | Inviting people from the web app |
| Machines | Pairing with a key fingerprint check; online status; per-workspace agent access; error report choice; a note when a machine's core is too old for the server | |
| Agent sign-in | The sealed sign-in terminal in a machine's settings; `weacon login` in a terminal | |
| Chats | **New chat** picks a machine, a repository and an agent; the server passes the prompt to that machine and keeps what the agent does; the chat screen shows it | |
| Agents | Claude Code and Codex | |
| Terminals | **Open terminal** in a chat, once the machine's owner allows it there; `weacon resume` to carry on a chat's agent in a terminal | |
| Tasks and docs | Adding, editing, statuses, doc revisions | |
| Proposals | Accepting and rejecting; agents propose through their core; while an agent works in a repository, the core proposes what a new commit in its folder does to docs that pin its code | |
| Search | ⌘K finds docs, tasks and chats by their words, and the code's symbols on your machine | |
| Code map | `weacon map` and the local MCP the core gives its agents | |
| Server MCP | | An MCP endpoint for agents Weacon didn't start |
| Chrome extension | Builds and registers with the core | |
| Updates | `weacon update`, `weacon update check` and `weacon update schedule`, which check a release's signature | A signed release to install: none is published yet |

## Run it on your machine

These steps run the server, a TLS front for it and the core on one Mac or Linux machine, for development and testing. Every piece keeps its data in the `.dev/` folder of your clone.

### 1. Install the tools

Install [mise](https://mise.jdx.dev). It provides the Node and Rust versions the project pins in `.node-version` and `rust-toolchain.toml`, and the scripts find them without a line in your shell profile.

On macOS, with [Homebrew](https://brew.sh):

```bash
xcode-select --install
brew install mise
```

On Linux (Debian or Ubuntu), install git, a C compiler and curl, then mise with its own install script, which puts it in `~/.local/bin`:

```bash
sudo apt-get install -y git build-essential curl
curl https://mise.run | sh
```

To run an agent, install it and sign in to it yourself, with the vendor's own instructions. Weacon never installs or signs in an agent for you without asking.

### 2. Get the code and set it up

Clone into a short path, such as `~/code/weacon.io`: the core's socket lives under the clone, and macOS limits a socket's path to 103 bytes.

```bash
git clone https://github.com/lcodes-dev/weacon.io.git ~/code/weacon.io
cd ~/code/weacon.io
scripts/setup.sh
```

To run `cargo` or `npm` yourself, put `mise exec --` before it, or activate mise in your shell and run `mise trust` once in the clone.

The first run takes a while: mise installs the pinned Node and Rust, then the script fetches the dependencies, builds the check tools and installs the web app's and the extension's packages and the Chromium the web app's tests drive. Its log is in `.dev/setup.log`. It is safe to run again, and `scripts/dev.sh` asks you to when the web app's packages are missing or out of date.

### 3. Start Weacon

Give your own email the first time. The first account on a server is its owner.

```bash
scripts/dev.sh you@example.com
```

It builds the web app, the server, the core and a TLS front, then starts them and prints:

```text
Weacon is running. Ctrl-C stops it.
  Web app:        http://localhost:8080
  Sign-in codes:  shown here, in each mail the server prints
  Pair the core:  WEACON_STATE_DIR=.dev/state target/debug/weacon pair https://localhost:8443 --pin .dev/tls/cert.pem
  Logs:           .dev/server.log, .dev/tls.log, .dev/core.log and .dev/state/logs/
```

Leave it running, and open a second terminal in the same folder for the next steps. Later runs need no email: `scripts/dev.sh`.

`dev-tls` is there because the core only dials a server over TLS it can verify, and the server speaks plain HTTP. It makes a certificate for `localhost` once, in `.dev/tls/`, and the core pins it. It is for development only: a real deployment puts a reverse proxy with a real certificate in front of `weacon-server`.

### 4. Sign in

Open <http://localhost:8080> in Chrome, enter your email and choose **Send code**. Nothing sends email yet, so the server prints the mail to its log instead, and `scripts/dev.sh` shows it in the first terminal:

```text
Mail to you@example.com: Your Weacon sign-in code
  Your code is ABCD-EFGH. It works for 10 minutes; if you didn't ask for it, ignore this mail.
```

Enter the code and choose **Sign in**. You see **Workspaces**, with your **Personal** workspace. Make more under **New workspace**.

### 5. Pair the core

In the second terminal:

```bash
export WEACON_STATE_DIR="$PWD/.dev/state"
target/debug/weacon pair https://localhost:8443 --pin .dev/tls/cert.pem --name "Dev laptop"
```

It prints a code and a key fingerprint, then waits. In the web app, open **Settings**, enter the code under **Pair a machine** and choose **Look up**. Check that the fingerprint matches the terminal's, then choose **Confirm**. The terminal says `Paired.`

Within 30 seconds the daemon connects, and the machine's row shows its version. Check from the terminal:

```bash
target/debug/weacon doctor
```

### 6. Set up the machine for a workspace

Open your workspace, then **Settings**, then the machine's **Settings**. There you choose:

- **Error reports**: off until you turn them on.
- **Agent access**: what the workspace's agents may do on this machine: off, read only, ask first or full.
- **Sign-ins**: each agent's state. **Sign in** opens the vendor's own sign-in in a terminal sealed between the browser and your core. The server only relays bytes it can't read. But the server also serves the web app's code, so this trusts whoever runs the server; if you don't, sign in over SSH on the machine instead, with the command below.

To sign in from a terminal, on the machine or over SSH, use the workspace's id, the `workspace=` part of the web app's address:

```bash
target/debug/weacon login claude --workspace <workspace id>
```

Give the workspace a repository its chats may work in, by its absolute path:

```bash
target/debug/weacon repo add --workspace <workspace id> demo "$HOME/code/demo"
target/debug/weacon repo list --workspace <workspace id>
```

### 7. Start a chat

Sign the agent in first, as in step 6, and give the workspace a repository.

1. Open your workspace and choose **New chat** in the left column.
2. Pick the **Machine**, the **Repository** and the **Agent**.
3. Type what you want done and choose **Send**.

The chat opens with your prompt at the top. The server passes the prompt to your machine, and the core starts the agent in a folder of its own for this chat. Each of the agent's messages appears under **Agent** once it is complete. In the left column, the chat shows who started it, the machine and what it's doing, such as **Waiting for a person**.

When the agent asks to use a tool, the chat names the tool, such as "Asks to use Bash", with **Allow** and **Deny**; with the access level at ask first, it waits for you. After a reload, a prompt still waiting says "Asks to use a tool" until the agent's next prompt, because the tool's name is passed on as it happens and never kept. It never shows the command or the file's contents, because a tool's input doesn't leave your machine, so you can't see them before you answer. Once the agent has stopped running, you can carry the chat on, on the machine itself (step 8).

**Stop** ends the agent's turn. Send another prompt in the same chat to carry on. Reloading the page keeps the chat as it was.

The server keeps only a chat's messages: the prompts, the agent's text, the run's state and its permission prompts, without the tool's name. Tool calls and changes aren't kept, so the chat shows text only. Only the person who started a chat can prompt it; the workspace's other members can read it. The machine must be connected when you send: if it isn't, the prompt is refused and nothing waits to run later, so send it again once the machine is back.

While the agent isn't signed in on that machine, the form says "Claude Code isn't signed in on this machine." and **Send** stays off. **Sign in from Settings** opens Settings with that machine's settings open: choose **Sign in** next to Claude Code, then **Back**. A Codex that isn't installed shows "Codex isn't installed on this machine."

Codex works differently from Claude Code: it asks you before every file change and every command that isn't read-only, whatever the workspace's access level. Weacon's start-of-session hook for it runs only after you have reviewed it once in Codex.

### 8. Try the rest

- **Tasks**: type a title under **New task** and choose **Add**. Change its status from the list.
- **Docs**: **New doc**, then a title and text, then **Save**. Each edit is a new revision.
- **Proposals**: changes to docs and tasks wait here until someone accepts or rejects them. Agents propose them from their chats. While an agent works in a repository, the core also proposes what a new commit in the repository's folder did to docs that pin its code.
- **Resume**: once a chat's agent has stopped running, `target/debug/weacon resume --workspace <workspace id> --chat <chat id>` carries it on in your terminal, on the chat's machine. There you answer the agent's own prompts yourself, whatever the workspace's access level. End that session before you send the chat another prompt from the web app, because the resumed agent holds the chat until then.
- **Open terminal**: a chat's **Open terminal** opens its folder in your own terminal on that machine. It needs a desktop terminal on that machine; on a machine without one, such as a server, use **Resume** there instead. On Linux the terminal runs the resume itself, so you see the agent there and answer its prompts there. It's off until you allow it, on that machine:

```bash
target/debug/weacon terminals on
```

- **Search**: press ⌘K (Ctrl+K on Linux and Windows), then type. It finds docs, tasks and chats by their words, and the code's symbols on your machine.
- **Teammates**: the operator adds them from the command line. Each signs in with their own email and pairs their own machine. With the server running:

```bash
target/debug/weacon-server create-workspace --db .dev/server/weacon.db --name "Team"
target/debug/weacon-server add-member --db .dev/server/weacon.db --workspace <workspace id> --email grace@example.com
target/debug/weacon-server members --db .dev/server/weacon.db --workspace <workspace id>
```

`create-workspace` prints the new workspace's id. Add yourself too, to see it in the web app.

- **Code map**: what the core's agents get through its local MCP. From the clone, about this repository or, with `--root`, any other:

```bash
target/debug/weacon map
target/debug/weacon map search pairing code
target/debug/weacon map hotspots
target/debug/weacon map --root "$HOME/code/demo"
```

### Stop, data and starting over

Ctrl-C in the first terminal stops the server, the TLS front and the core. `scripts/dev.sh` starts them again, with your account, workspaces and pairing as they were.

| Folder | What it holds |
| --- | --- |
| `.dev/server/` | The server's database and its key for pairing codes |
| `.dev/tls/` | The TLS front's certificate and key |
| `.dev/state/` | The core's state: its pairing, settings, sign-in folders and logs |
| `.dev/work/` | The folders agents work in |

To start over, stop Weacon, then remove the server's and the core's data together, since a core paired with a server you removed can't connect. If you paired a second core for the Chrome extension, pair it again afterwards, as in that section. If you set `WEACON_STATE_DIR` to a folder of its own, such as `~/.weacon-dev/state`, remove that folder too. The core keeps agents' work in a `work` folder beside it, such as `~/.weacon-dev/work`: check that it holds only Weacon's folders before you remove it.

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

In Chrome, open `chrome://extensions`, turn on **Developer mode**, choose **Load unpacked** and pick `apps/extension/dist`. Chrome starts the core's native host without your shell's variables, so the extension reaches the core in its default state folder, not the one in `.dev/state`. Run a second core there, in a third terminal, without `WEACON_STATE_DIR`:

```bash
unset WEACON_STATE_DIR
target/debug/weacon daemon
```

Then, in another terminal, pair it as a second machine and let Chrome reach it:

```bash
unset WEACON_STATE_DIR
target/debug/weacon pair https://localhost:8443 --pin .dev/tls/cert.pem --name "Dev laptop, Chrome"
target/debug/weacon connect-chrome
```

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

- **The daemon doesn't start, with `daemon.start_failed`.** The core's socket is `weacon.sock` in its state folder, and a socket's path holds at most 103 bytes on macOS and 107 on Linux. Clone into a shorter path, or set `WEACON_STATE_DIR` to a short folder, such as `~/.weacon-dev/state`, for `scripts/dev.sh` and every `weacon` command. `scripts/dev.sh` checks the length first.
- **`scripts/setup.sh` says Weacon needs another Node, or to install mise.** Install mise as in step 1, then run it again. Without mise, the Node on your `PATH` must be exactly the one in `.node-version`, and rustup must be installed.
- **`scripts/dev.sh` says the server didn't start.** Something else uses port 8080, or 8443 for the TLS front. Stop it, then run again. Read `.dev/server.log` and `.dev/tls.log`.
- **The machine shows no version after pairing.** The daemon looks for its pairing every 30 seconds. Check that `scripts/dev.sh` is still running, then read `.dev/state/logs/daemon.log`.
- **`update check failed` in the core's log.** There is no release to check against yet. Turn the checks off with `target/debug/weacon update schedule off`.
- **Nobody can sign in.** If the first run stopped before it made your account, run `scripts/dev.sh you@example.com` again: it makes the account when it's missing.
- **The sign-in page never accepts the code.** Use the newest mail `scripts/dev.sh` shows, and request a new code if it expired.

## Licence

MIT. See [`LICENSE`](LICENSE).
