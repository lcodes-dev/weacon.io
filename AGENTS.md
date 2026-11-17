# Weacon: notes for coding agents

Weacon is a web harness for coding agents with tasks, docs and team chats built in. One Rust server holds the data in SQLite and serves a React web app from the same binary. `docs/architecture.md` says how it fits together.

- **Keep it simple.** Write the shortest code that reads in one pass. Build nothing ahead of the feature that needs it: no abstractions, options or dependencies for later.
- **Comment only when strictly necessary.** A comment says why, or something the code can't show, in one short line. Never restate what the code does.
- **One word per thing.** A group of people is a "team", never a "workspace"; the first account on a server is its "owner".
- **`.legacy/` is the previous codebase.** Read it for reference; never edit it or build on it.
- **`.planning/reference_ui.html`** is the clickable prototype the web app follows.
- **Plans** live in `.planning/` as numbered files with a status line and a checklist of steps; a plan is done when every step is ticked. Questions for the owner go in `.planning/open_questions.md` and improvements nobody asked for in `.planning/improvements.md`, never in a plan.
- **Tests are integration tests.** The server's tests live in `apps/server/tests/` and drive the API over HTTP, as a browser would. Write a unit test inside `src/` only for something no request can reach.
- **Docs** live in `docs/` as Markdown and are updated in the same change as the code they describe.

## Commands

Node and Rust come from mise at the versions in `mise.toml`. In a shell without mise activated, put `mise exec --` before a command.

| Command | What it does |
| --- | --- |
| `scripts/dev.sh` | Installs the pinned tools and the web app's packages, then runs the server on `http://localhost:8080` and the web app, with hot reload, on `http://localhost:5173` |
| `npm --prefix apps/web run build` | Type-checks and builds the web app into `apps/web/dist/` |
| `cargo test -p weacon-server` | Runs the server's tests in `apps/server/tests/`; build the web app first |

## UI SDK

Before using ui-sdk, follow the ui-sdk skill if it's installed, and read `apps/web/node_modules/@ui-sdk/react/docs/llms.txt`. To build a new component, also search `apps/web/node_modules/@ui-sdk/react/docs/core.md` by heading, and read `rendering.md` next to it.
