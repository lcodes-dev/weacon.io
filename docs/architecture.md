# Architecture

Weacon is one Rust server that holds the data in SQLite and serves a React web app from the same binary.

## Layout

```
apps/
  server/        the Rust server: axum, tokio, sqlx
    src/         lib.rs holds the router; main.rs only starts it
    assets/      files built into the binary: the logo for mail
    migrations/  numbered .sql files
    tests/       the server's tests
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

## Logging

The server logs with the `tracing` crate to its standard output: a timestamp, a level, where the line came from, and the message.

- Every API request gets one line with its method, path, status and duration. The path is shown without its `/api` prefix. The health check and the web app's files are left out, since they would drown the rest.
- A line logged while a request is handled carries that request's method and path.
- The level is `info` unless the `RUST_LOG` environment variable says otherwise. `RUST_LOG=debug` also shows what sqlx, lettre and axum log, such as each SQL query.

## Tests

The server's tests are integration tests in `apps/server/tests/`. Each one builds the router over an in-memory database and sends it HTTP requests, cookie included, so it checks what a browser would see. The server is a library (`lib.rs`) with a small binary (`main.rs`) so that those tests can import the router.

There are no unit tests inside `src/`. One is added only for something no request can reach.

## Database

- **SQLite only**, through `sqlx` with the bundled SQLite.
- One database file; its path comes from a flag or an environment variable.
- WAL mode and foreign keys on.
- Migrations are plain `.sql` files in `apps/server/migrations/`, embedded in the binary and run at startup. A schema change is a new numbered file; a file that has been committed is never edited.
- Queries are checked at runtime, not with sqlx's compile-time macros, so a build never needs a live database.
- All SQL lives in one `db` module.

## Web app

- **React** with TypeScript, built by **Vite**.
- **UI SDK** (`@ui-sdk/react` and `@ui-sdk/core`, from ui-sdk.dev) for components. Its components are unstyled: they bring keyboard, focus, popup and screen reader behavior, and we style them. Where it has no component for what we need, we build one from its behaviors.
- **Tailwind** for all styling, following the reference. UI SDK's parts are targeted through their `data-slot` and state attributes.
- UI SDK is a beta a few weeks old, so its version is pinned exactly and upgraded on purpose.
- **React Router** for client-side routes. Settings are pages under `/settings/`, shown beside the sidebar.
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

## Accounts, teams and projects

- **Accounts.** Anyone can register. People sign in with a 6-digit code sent to their email; there are no passwords. A code lasts 10 minutes and dies after 5 wrong tries.
- **The owner** is the first person to register on a server. They own the server, not a team, and they set its mail settings. Their first sign-in code is in the server's log, because no SMTP is set yet.
- **Teams.** Registering gives a person a team of their own, named after them. The word is always "team", never "workspace". A team has one owner, the person who created it, who renames it, invites people and removes them. A member can leave; the owner can't, so everyone always has a team.
- **Invites.** The team's owner invites an email address. The mail holds a link, the server's address plus `/invite/` and a random token saved with the invite. The link works once, for 7 days, and only for an account with the invited address. The owner can invite several addresses at once, revoke an invite, or resend it; resending is inviting the same address again, which replaces the token and starts the 7 days over.
- **Several teams.** The server doesn't pick a person's team: the session lists their teams, and every team route names its team in the path. The web app remembers which team is open in the browser's local storage. A team the person isn't in answers 404.
- **Projects.** A team holds several projects. A project is a name and, optionally, a GitHub repository stored as `owner/name`. Any member creates and edits projects; only the team's owner deletes one. Nothing else belongs to a project yet.
- **Sessions.** Signing in sets an `HttpOnly` cookie holding a random token that names a row in `sessions`.

## GitHub

Each team has its own private GitHub App, registered and installed by the team's owner from "Team settings". `github.rs` holds every call to GitHub.

- **Registering.** The web app posts an app manifest to GitHub from a form. GitHub sends the owner back to `/settings/team` with a one-time code, and the server trades it at `POST /app-manifests/{code}/conversions` for the app's id, name and private key, saved on the team's row. The web app keeps a random `state` in the browser tab and ignores a code that comes back without it.
- **Installing.** "Choose repositories" opens the app's install page on GitHub. GitHub lets a private app be installed only on the account that owns it, so an app has at most one installation and it belongs to the team that registered it. The server stores nothing about it.
- **Calling GitHub.** The server signs a short-lived token with the private key, asks for the app's installation, trades for an installation token, and lists the repositories. A project's repository must be in that list. Nothing is kept between requests.
- **Access.** The app may read the code of the chosen repositories and nothing else. It has no webhook, so GitHub never calls the server.
- GitHub's API address is part of the router's state, so the tests point it at a fake GitHub they start themselves.

## Code map

A project tracks chosen branches of its repository, and the server keeps a map of each: every file, the symbols defined in it, and which file uses which. `map.rs` holds it.

- **The queue.** A row of `branches` has `head_sha`, the latest commit GitHub has, and `commit_sha`, the commit its map describes. A branch waits to be mapped while they differ.
- **The worker.** One task, started by `main.rs`, runs a round every minute and at once when a project's branches change. A round asks GitHub for each tracked repository's branches and notes their commits, then maps every waiting branch.
- **Mapping.** The server asks GitHub for the commit's files, which come with git's id of each content. Contents it has already read for the project are skipped. If any is new, it downloads the commit as one archive and reads only the new ones. The branch's file list and commit are saved in one transaction.
- **By content.** What is read from a file is stored against git's id of its content (`blobs`, `blob_symbols`, `blob_mentions`), so a version of a file is read once, across commits and branches. `branch_files` says which contents a branch has. Contents no branch lists are deleted.
- **Reading.** tree-sitter parses a file, and the tags query shipped with each grammar says which nodes define a name and which mention one. `LANGUAGES` in `map.rs` lists the languages: JavaScript, TypeScript, Rust, Python and PHP. Other files are listed with their line count only, and files over 500 KB or not text are not read.
- **Uses.** File A uses file B when A mentions a name that only B defines on that branch. It is matching by name, computed when the map is asked for.
- **A new reader.** `READER` in `map.rs` is raised when what is read changes, such as with a new language. At startup a server with another number forgets every map, and the worker reads them all again.
- **The picture.** The web app draws a branch as a graph: a dot is a file, inside the circle of its folder, and a line joins a file to one it uses, bent toward the folder both share so that lines gather into bundles. `d3-hierarchy` packs the circles once, so nothing moves afterwards but the view: the wheel zooms, dragging moves, and clicking a folder fits it. Clicking a file shows its symbols and lights up its lines, solid to what it uses and dashed from what uses it. The layout is worked out in the browser and saved nowhere.
- The server asks GitHub; GitHub never calls it. A webhook would only have to write `head_sha`.

## Mail

`mail.rs` has one way to send: `Mailer::send(to, mail)`, where a `Mail` is a subject, some text, an optional code to show large and an optional button that opens an address.

- Every mail goes out as HTML with a plain-text fallback, both made from the same `Mail`.
- The HTML is one layout, the `TEMPLATE` string in `mail.rs`: the logo, a card with the subject, text, code and button, and a footer. It is filled by replacing `{subject}`, `{body}` and `{footer}`.
- The logo is attached to each mail and shown inline, so it needs no public address. Mail clients don't show SVG, so `apps/server/assets/logo.png` is made from `logo.svg` beside it: `sed 's/viewBox="0 0 640 640"/viewBox="96 96 448 448"/' logo.svg | rsvg-convert -w 128 -h 128 -o logo.png`.

- The owner's SMTP settings are the one row of the `config` table. The server reads it at startup and keeps a ready connection in memory; saving new settings replaces it, with no restart.
- A link in a mail starts with the `WEACON_URL` setting, the address people open in their browser.
- With no SMTP settings, the plain-text part of every mail is printed to the server's log, codes included.
- The owner edits the settings on the web app's "Mail settings" page, which can also send a test mail.

## API

Errors are `{"error": "..."}` with a status code.

| Route | Who | What it does |
| --- | --- | --- |
| `GET /api/health` | anyone | Says the server and its database answer |
| `GET /api/session` | anyone | `{"setup_needed", "user"}`: whether the server has no accounts yet, and who is signed in |
| `POST /api/sign-in/code` | anyone | Mails a code. With a `name` it is a registration; without, only a known account gets one |
| `POST /api/sign-in/verify` | anyone | Checks the code, creates the account and its team if it was a registration, and signs in |
| `POST /api/sign-out` | signed in | Ends the session |
| `GET`, `PUT`, `DELETE /api/config/mail` | owner | Reads, saves or clears the SMTP settings. The password is never read back, and saving without one keeps it |
| `POST /api/config/mail/test` | owner | Sends the owner a test mail and answers with the SMTP error if it fails |
| `GET /api/teams/{id}` | a member | The team's name, members and open invites |
| `PUT /api/teams/{id}` | the team's owner | Renames the team |
| `POST /api/teams/{id}/invites` | the team's owner | Invites the `emails` and mails each its link. No mail goes out if one address is wrong or already a member |
| `DELETE /api/teams/{id}/invites/{invite_id}` | the team's owner | Revokes an invite |
| `DELETE /api/teams/{id}/members/{user_id}` | the team's owner, or that member | Removes a member, or leaves. Refused for the owner |
| `GET`, `POST /api/teams/{id}/projects` | a member | Lists the team's projects with their tracked branches, or creates one from `name`, an optional `repo` and the `branches` to track |
| `PUT /api/teams/{id}/projects/{project_id}` | a member | Saves the name, repository and tracked branches; leaving `repo` out removes it |
| `DELETE /api/teams/{id}/projects/{project_id}` | the team's owner | Deletes the project |
| `GET /api/teams/{id}/projects/{project_id}/map?branch=` | a member | A branch's map: its commit, every file with its symbols, and which file uses which |
| `GET /api/teams/{id}/github/branches?repo=` | a member | A repository's branches and which is the default |
| `GET /api/teams/{id}/github` | a member | The team's GitHub App, the account it is installed on, and the repositories it reaches |
| `POST /api/teams/{id}/github` | the team's owner | Trades GitHub's `code` for the new app and saves it |
| `DELETE /api/teams/{id}/github` | the team's owner | Forgets the app; it stays on GitHub |
| `GET /api/invites/{token}` | anyone | The invite's team, inviter and address, and `has_account`, whether that address has an account; 404 once used, revoked or expired |
| `POST /api/invites/{token}/accept` | signed in with the invited address | Joins the team and uses the invite up |

## Decided against, for now

- **Postgres.** Every migration twice and a restricted SQL dialect, for a need we don't have.
- **Generated API types** (utoipa and OpenAPI). Worth revisiting with a second client or around a hundred endpoints.
- **Inertia.** No official Rust adapter, and most of the UI is live state, not page visits.
- **Svelte, Vue, Leptos, htmx.** The reference UI is React, and the libraries for diffs, editing and chat are strongest there.
- **shadcn.** UI SDK fills the same role, and running both would mean two sets of component primitives.

## What exists today

A fresh server opens on "Set up Weacon". After that people register and sign in with emailed codes, each account has its own team, the owner can set up SMTP, and a team's owner can rename it, invite people by email and set up the team's GitHub App, and members make projects, with or without a repository. A project with a repository has a code map of each branch it tracks. The shell is still the empty layout from the reference.

Not built yet, each waiting for the first feature that needs it: the WebSocket.
