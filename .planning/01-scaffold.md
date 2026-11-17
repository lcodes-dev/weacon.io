# Plan 01: scaffold

**Status: done** (2026-11-17)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: `./dev.sh` starts everything, and the browser shows the empty shell from the reference with a live answer from the server's API. Nothing else.

## Files

About 15 files, roughly 150 lines of Rust and 150 of TypeScript.

```
.gitignore            target/, node_modules/, web/dist/, .dev/
README.md             what Weacon is, how to run it (short)
AGENTS.md             a few lines: keep it simple, and UI SDK's note for agents
CLAUDE.md             one line that imports AGENTS.md
dev.sh                starts the server and Vite, stops both on Ctrl-C

server/
  Cargo.toml
  src/main.rs         read settings, open the database, build the router, serve
  src/db.rs           open the SQLite file with WAL and foreign keys on
  src/web.rs          serve the embedded web app, with the index.html fallback

web/
  package.json
  vite.config.ts      React, Tailwind, and the proxy for /api and /ws
  tsconfig.json
  index.html
  src/main.tsx        mounts the app inside TanStack Query's provider
  src/app.css         Tailwind, plus the reference's colors as CSS variables
  src/api.ts          one `get<T>(path)` helper and the `Health` type
  src/Shell.tsx       sidebar and main pane, static, with the server's status
```

## Server

**Dependencies (5):** `axum`, `tokio`, `sqlx` (SQLite), `rust-embed`, `serde`. One dev-dependency, `tower`, to call the router in tests.

**Settings:** two environment variables with defaults, no argument parser.

| Variable | Default | Meaning |
| --- | --- | --- |
| `WEACON_ADDR` | `127.0.0.1:8080` | Where the server listens |
| `WEACON_DB` | `weacon.db` | The database file, created if missing |

**Routes:**

| Route | Answer |
| --- | --- |
| `GET /api/health` | `{"status":"ok"}` after a `SELECT 1` against the database |
| Other paths under `/api` | JSON 404 |
| Everything else | An embedded file, or `index.html` |

**State:** the router's state is the `SqlitePool` itself. No wrapper struct until there is a second thing to put in it.

**Errors:** `main` returns a `Result`; a startup failure prints the error and exits. No error type of our own yet.

**Tests (3):** health answers ok; an unknown `/api` path is a JSON 404; an unknown other path returns `index.html`.

## Web app

**Dependencies:** `react`, `react-dom`, `@tanstack/react-query`, `@ui-sdk/react`, `@ui-sdk/core` (pinned exactly), and for the build `vite`, `@vitejs/plugin-react`, `tailwindcss`, `@tailwindcss/vite`, `typescript` and React's types. npm as the package manager.

**What it shows:** the reference's layout as static markup: the sidebar with its section headings and no items, and an empty main pane. A small dot in the sidebar is green when `GET /api/health` answers and red when it doesn't. That call is the one use of TanStack Query and of `api.ts`.

**UI SDK setup:** install the two packages, add its note to `AGENTS.md`, and install its `ui-sdk` skill. The shell uses a UI SDK component only where it needs one.

## dev.sh

About 10 lines: start `cargo run` with `WEACON_DB=.dev/weacon.db`, start `npm run dev`, and stop both on Ctrl-C. It runs `npm install` first if `web/node_modules` is missing.

A release build is two commands in the README: `npm run build` in `web/`, then `cargo build --release` in `server/`. No release script yet.

## Left out on purpose

Each of these arrives with the first feature that needs it.

| Left out | Arrives with |
| --- | --- |
| Migrations (`server/migrations/` and the call that runs them) | The first table |
| React Router | The second screen |
| The WebSocket at `/ws` | The first live update |
| Accounts and sign-in | Its own plan |
| Logging beyond one "listening on" line | The first thing worth logging |
| A Cargo workspace | A second crate |
| CI, lint configuration, a release script, Docker | When you ask |
| Web tests | The first logic worth testing |

## Steps

- [x] Server crate: settings, database, routes, embedded web app
- [x] Server tests (3)
- [x] Web app: Vite, React, Tailwind, the shell and the status dot
- [x] UI SDK: packages pinned, note in `AGENTS.md`, skill installed
- [x] `dev.sh`, `README.md`, `AGENTS.md`, `CLAUDE.md`, `.gitignore`
- [x] Checked in the browser: the shell renders and the dot is green

## Outcome

What changed from the plan above while building, and after it:

- `rust-embed` needs `web/dist/` to exist for the server to compile. An empty folder is enough, so `dev.sh` creates it; no web build is needed first.
- The fallback test reads the real `index.html`, so the web app must be built before the server's tests run.
- The shell uses no UI SDK component yet; a native `button` was all it needed.
- Both the light and the dark palette from the reference are in, following the system setting.
- `dev.sh` now also does the setup: it installs Node and Rust through mise at the versions in `mise.toml`, and the web app's packages.
- `dev.sh` moved to `scripts/dev.sh`.
- The layout moved to `apps/server` and `apps/web` under a root Cargo workspace, so the paths above are the planned ones, not today's.

Open questions are in [open_questions.md](open_questions.md), and possible improvements in [improvements.md](improvements.md).
