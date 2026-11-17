# Architecture

Weacon is one Rust server that holds the data in SQLite and serves a React web app from the same binary.

## Layout

```
apps/
  server/        the Rust server: axum, tokio, sqlx
    migrations/  numbered .sql files
  web/           React, TypeScript, Vite, Tailwind, UI SDK
crates/          Rust code shared between apps; none yet
docs/            these docs
.planning/       reference_ui.html, the clickable prototype the UI follows
.legacy/         the previous codebase, for reference only
```

The root `Cargo.toml` is a workspace, so `cargo` commands run from the root and build into one `target/`. Today it has one member, the server. `crates/` appears with the first code a second app needs; until then code stays in the app that uses it.

## Server

- **axum** on tokio.
- JSON API under `/api/...`.
- One WebSocket at `/ws` for live updates.
- Everything else is the web app (see below).

## Database

- **SQLite only**, through `sqlx` with the bundled SQLite.
- One database file; its path comes from a flag or an environment variable.
- WAL mode and foreign keys on.
- Migrations are plain `.sql` files in `apps/server/migrations/`, embedded in the binary and run at startup.
- Queries are checked at runtime, not with sqlx's compile-time macros, so a build never needs a live database.
- All SQL lives in one `db` module.

## Web app

- **React** with TypeScript, built by **Vite**.
- **UI SDK** (`@ui-sdk/react` and `@ui-sdk/core`, from ui-sdk.dev) for components. Its components are unstyled: they bring keyboard, focus, popup and screen reader behavior, and we style them. Where it has no component for what we need, we build one from its behaviors.
- **Tailwind** for all styling, following the reference. UI SDK's parts are targeted through their `data-slot` and state attributes.
- UI SDK is a beta a few weeks old, so its version is pinned exactly and upgraded on purpose.
- **React Router** for client-side routes.
- **TanStack Query** for fetching and caching API data.
- API types are **written by hand** in TypeScript, next to the fetch functions that use them. No OpenAPI, no generation.

## How the server serves the web app

`vite build` writes `apps/web/dist/`. The server embeds that folder with `rust-embed` and serves it from a fallback route:

| Request | Answer |
| --- | --- |
| A path under `/api` or `/ws` that no route matches | JSON 404 |
| A path that matches an embedded file | That file |
| Anything else | `index.html`, so a client-side route such as `/chats/42` survives a refresh |

Caching: `index.html` is never cached; the hashed files under `/assets/` are cached for a year.

In a debug build `rust-embed` reads `apps/web/dist/` from disk, so the server doesn't recompile when the web app is rebuilt. The folder must exist for the server to compile, even empty; `scripts/dev.sh` creates it.

## Development

`scripts/dev.sh` does the setup and starts everything. It installs Node and Rust through mise at the versions pinned in `mise.toml`, installs the web app's packages, then starts two processes:

- the server on port 8080, serving `/api` and `/ws`;
- Vite's dev server on port 5173, with hot reload, proxying `/api` and `/ws` to 8080.

The browser opens Vite's port, so everything is one origin: no CORS setup, and cookies behave as in production.

## Release

Build the web app, then the server: `vite build`, then `cargo build --release`, both with mise's pinned tools. The result is one binary holding the API, the migrations and the web app. Node is needed only to build.

## Live updates

- Most changes (tasks, docs, lists) reach the browser as small "this changed" events on the WebSocket, carrying no data. The web app refetches through the API, so HTTP stays the one source of data.
- Streaming content, such as an agent's transcript, travels on the socket itself.

## Decided against, for now

- **Postgres.** Every migration twice and a restricted SQL dialect, for a need we don't have.
- **Generated API types** (utoipa and OpenAPI). Worth revisiting with a second client or around a hundred endpoints.
- **Inertia.** No official Rust adapter, and most of the UI is live state, not page visits.
- **Svelte, Vue, Leptos, htmx.** The reference UI is React, and the libraries for diffs, editing and chat are strongest there.
- **shadcn.** UI SDK fills the same role, and running both would mean two sets of component primitives.

## What exists today

The server starts, creates the database file, answers `GET /api/health`, and serves a web app that shows the empty shell from the reference (sidebar and main pane) with a dot that is green while the API answers.

Not built yet, each waiting for the first feature that needs it: migrations, React Router, the WebSocket, sign-in.
