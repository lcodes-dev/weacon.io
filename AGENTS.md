# Weacon: notes for coding agents

Weacon is a web harness for coding agents with tasks, docs and team chats built in. One Rust server holds the data in SQLite and serves a React web app from the same binary. A second small program, the core, runs on each person's own machine and runs their agents there. `docs/architecture.md` says how it fits together.

- **Keep it simple.** Write the shortest code that reads in one pass. Build nothing ahead of the feature that needs it: no abstractions, options or dependencies for later.
- **Comment only when strictly necessary.** A comment says why, or something the code can't show, in one short line. Never restate what the code does.
- **One word per thing.** A group of people is a "team", never a "workspace"; the first account on a server is its "owner"; a person's own computer is a "machine", and the program on it is the "core".
- **`.legacy/` is the previous codebase.** Read it for reference; never edit it or build on it.
- **`.planning/reference_ui.html`** is the clickable prototype the web app follows.
- **Plans** live in `.planning/` as numbered files with a status line and a checklist of steps; a plan is done when every step is ticked. Questions for the owner go in `.planning/open_questions.md`, and are deleted from it once answered. Improvements nobody asked for go in `.planning/improvements.md`. Neither goes in a plan.
- **Queries are checked at build time.** Write each one with sqlx's `query!`, `query_as!` or `query_scalar!`, in the area's file of `apps/server/src/db/`, never as a string built at run time. After changing a query or a migration, run `scripts/prepare-queries.sh` and commit `.sqlx/` with the change.
- **The server's layers.** `routes.rs` lists the addresses, `http/` holds thin handlers, `db/` the queries, one file per area for each. Work that outlasts a request is a job in `jobs/`, dispatched with `jobs::dispatch`, never a bare `tokio::spawn`. A handler returns `Result<T>` and fails with an `ApiError` constructor such as `ApiError::not_found()`.
- **Tests are integration tests.** The server's tests live in `apps/server/tests/` and drive the API over HTTP, as a browser would. Write a unit test inside `src/` only for something no request can reach. A crate's tests live in its own `tests/` and call it as the server does.
- **A crate for what is complex.** A piece of work with its own rules goes in `crates/`, behind a few functions, so that the app calling it stays simple. Whether a second app needs it doesn't matter.
- **Docs** live in `docs/` as Markdown and are updated in the same change as the code they describe.

## Commands

Node and Rust come from mise at the versions in `mise.toml`. In a shell without mise activated, put `mise exec --` before a command.

| Command | What it does |
| --- | --- |
| `scripts/dev.sh` | Installs the pinned tools and the web app's packages, then runs the server on `http://localhost:8080` and the web app, with hot reload, on `http://localhost:5173` |
| `scripts/prepare-queries.sh` | Writes `.sqlx/`, which the build checks every query against; run it after changing a query or a migration |
| `scripts/check.sh` | Checks what GitHub checks on every push: the format, the web app's build, the lints and every test |
| `npm --prefix apps/web run build` | Type-checks and builds the web app into `apps/web/dist/` |
| `npm --prefix apps/web run phone-check` | Runs the server on a fresh database and opens every page at a phone's size, checking that nothing scrolls sideways and everything to tap is 44px tall; build the web app first |
| `cargo test -p weacon-server` | Runs the server's tests in `apps/server/tests/`; build the web app first |
| `cargo test -p weacon-core` | Runs the core's test of the agents' programs |
| `cargo test -p agent-run` | Runs the agents' turns against a stand-in that prints what the real programs printed |
| `cargo test -p chat-copy` | Runs a chat's copy of a repository against one made in a temporary folder |
| `cargo test -p doc-search` | Runs the docs search against a few made-up pages, with the real model, which the first run fetches |
| `cargo test -p code-map` | Runs the code map's tests: a small made-up repository per language |
| `cargo run -p code-map --example map -- <folder>` | Prints which file uses which in a folder on this computer; `--example tree -- <file>` prints how a file is parsed and read |

## UI SDK

Before using ui-sdk, follow the ui-sdk skill if it's installed, and read `apps/web/node_modules/@ui-sdk/react/docs/llms.txt`. To build a new component, also search `apps/web/node_modules/@ui-sdk/react/docs/core.md` by heading, and read `rendering.md` next to it.
