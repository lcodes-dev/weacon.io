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
| `RUST_LOG` | `info` | How much the server logs; `debug` adds every SQL query and SMTP exchange |

## Docs

- [Architecture](docs/architecture.md)
