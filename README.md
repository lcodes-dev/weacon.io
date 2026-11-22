# Weacon

A web harness for coding agents with tasks, docs and team chats built in. One Rust server holds the data in SQLite and serves the web app from the same binary.

## Run it

You need [mise](https://mise.jdx.dev). It installs the Node and Rust versions pinned in `mise.toml`.

```bash
scripts/dev.sh
```

The first run installs the tools and the web app's packages. Then open http://localhost:5173. The server listens on port 8080 and keeps its database in `.dev/`.

The first person to register owns the server. Until the owner saves SMTP settings in the web app, every mail is printed to the server's log, so the 6-digit sign-in codes are in the terminal where `scripts/dev.sh` runs.

## Build a release

```bash
mise install
mise exec -- npm --prefix apps/web install
mise exec -- npm --prefix apps/web run build
mise exec -- cargo build --release -p weacon-server
```

The result is `target/release/weacon-server`, with the web app inside it.

| Variable | Default | Meaning |
| --- | --- | --- |
| `WEACON_ADDR` | `127.0.0.1:8080` | Where the server listens |
| `WEACON_DB` | `weacon.db` | The database file, created if missing |
| `WEACON_URL` | `http://localhost:8080` | The address people open in their browser, used for links in mail |
| `RUST_LOG` | `info` | How much the server logs; `debug` adds every SQL query and SMTP exchange |

## A machine of your own

Agents run on your own computer or virtual machine, on your own sign-in to Claude Code or Codex. The program that connects it to Weacon is the core:

```bash
mise exec -- cargo build --release -p weacon-core
```

Copy `target/release/weacon-core` to the machine, on macOS, Linux, or WSL on Windows. Then, in Weacon, open "Machines", press "Add a machine" and run there the command it shows. After that:

| Command | What it does |
| --- | --- |
| `weacon-core` | Connects, and stays connected |
| `weacon-core install` | Starts with the computer and runs in the background; `uninstall` undoes it |
| `weacon-core status` | Which Weacon this machine is paired with |
| `weacon-core agents` | Which agents are installed and signed in |
| `weacon-core add user@host` | Sets up a server of yours over SSH and pairs it |

Signing in to an agent is done from Weacon's "Machines" page.

"New chat" then has Claude Code or Codex work on a project there: in a copy of its repository in the core's folder, on your sign-in, with commands kept inside the agent's own sandbox. The machine needs `git`.

## Docs

- [Architecture](docs/architecture.md)
