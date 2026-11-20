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
  core/          the core: the program on a person's own machine
    src/         main.rs holds its commands; link.rs its connection; agents.rs the vendors' programs
  web/           React, TypeScript, Vite, Tailwind, UI SDK
    src/         main.tsx holds the session and the top routes; api.ts the API's types
      routes/      one file per address, such as Project.tsx for /projects/:id
      components/  what routes are built from, such as Shell.tsx, the sidebar around a signed-in route
crates/
  code-map/      reads a file's symbols and imports, and works out which file uses which
  core-link/     the messages the server and a core send each other
docs/            these docs
.planning/       reference_ui.html, the clickable prototype the UI follows
.legacy/         the previous codebase, for reference only
```

The root `Cargo.toml` is a workspace, so `cargo` commands run from the root and build into one `target/`. It has four members: the server, the core, and the `code-map` and `core-link` crates. A piece of work gets its own crate in `crates/` when it is complex enough to stand apart from the app that calls it: it has its own rules and its own tests, and the app only asks it for a result. Whether a second app needs it doesn't matter.

## Server

- **axum** on tokio.
- JSON API under `/api/...`.
- One WebSocket at `/ws` for live updates.
- Everything else is the web app (see below).

## Logging

The server logs with the `tracing` crate to its standard output: a timestamp, a level, where the line came from, and the message.

- Every API request gets one line with its method, route, status and duration. The route is the pattern that matched, such as `/api/invites/{token}`, never the address itself, which can hold a secret. The health check and the web app's files are left out, since they would drown the rest.
- A line logged while a request is handled carries that request's method and route.
- The level is `info` unless the `RUST_LOG` environment variable says otherwise. `RUST_LOG=debug` also shows what sqlx, lettre and axum log, such as each SQL query.

## Tests

The server's tests are integration tests in `apps/server/tests/`. Each one builds the router over an in-memory database and sends it HTTP requests, cookie included, so it checks what a browser would see. The server is a library (`lib.rs`) with a small binary (`main.rs`) so that those tests can import the router.

There are no unit tests inside `src/`. One is added only for something no request can reach.

## Database

- **SQLite only**, through `sqlx` with the bundled SQLite.
- One database file; its path comes from a flag or an environment variable.
- WAL mode and foreign keys on.
- At startup the server makes the file, and SQLite's two side files, readable by its own account only: it holds sessions, keys and source code.
- **Keys.** A row with an id of its own (a user, team, project, invite or docs page) has a UUIDv7 made by the server, stored as text. It starts with the time it was made, so ids sort by age and are never given out twice; in the API and in addresses an id is that text. A table whose rows are already named by something unique is keyed by that: a session by its token, a branch by its project and name, a file's content by its project and git's id.
- **A transaction that reads before it writes starts as a writer,** with `BEGIN IMMEDIATE`. Started the usual way, SQLite refuses its write when another request wrote in between, and the request fails. `update_project` is the only one.
- **Links do the clean-up.** A branch's files go with the branch, what was read from a content goes with the content, and everything of a project goes with the project.
- **Indexes.** Each query finds its rows through a key or an index; `EXPLAIN QUERY PLAN` in `sqlite3` shows how a new one runs. Small tables keyed by text are `WITHOUT ROWID`, so the key is the table and not a second tree beside it.
- Migrations are plain `.sql` files in `apps/server/migrations/`, embedded in the binary and run at startup. A schema change is a new numbered file; a file that has been committed is never edited.
- Every query is written with sqlx's `query!`, `query_as!` or `query_scalar!`, which check it against the schema when the server is built: a misspelt column or a wrong type stops the build. The build needs no database, because the checks are saved in `.sqlx/`, one file per query, kept in git.
- After changing a query or a migration, run `scripts/prepare-queries.sh`. It runs the migrations on an empty database in `target/`, writes `.sqlx/` again, and says what is wrong with a query that doesn't fit. Until then the build stops at that query.
- SQLite reports a true-or-false value as a number, so such a column is written `is_owner AS "is_owner: bool"`. A value SQLite can't tell is always there is written `repo AS "repo!"`.
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
| A path under `/assets/` that matches none | JSON 404 |
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

- **Accounts.** Anyone can register. People sign in with a 6-digit code sent to their email; there are no passwords. A code lasts 10 minutes and dies after 5 wrong tries. Asking for another code doesn't reset the tries while the earlier one is still live, and a second request for the same address within a minute changes nothing and sends nothing.
- **The owner** is the first person to register on a server. They own the server, not a team, and they set its mail settings. Their first sign-in code is in the server's log, because no SMTP is set yet.
- **Teams.** Registering gives a person a team of their own, named after them. The word is always "team", never "workspace". A team has one owner, the person who created it, who renames it, invites people and removes them. A member can leave; the owner can't, so everyone always has a team.
- **Invites.** The team's owner invites an email address. The mail holds a link, the server's address plus `/invite/` and a random token saved with the invite. The link works once, for 7 days, and only for an account with the invited address. The owner can invite several addresses at once, revoke an invite, or resend it; resending is inviting the same address again, which replaces the token and starts the 7 days over.
- **Several teams.** The server doesn't pick a person's team: the session lists their teams, and every team route names its team in the path. The web app remembers which team is open in the browser's local storage. A team the person isn't in answers 404.
- **Projects.** A team holds several projects. A project is a name and, optionally, a GitHub repository stored as `owner/name`. Any member creates and edits projects; only the team's owner deletes one. Nothing else belongs to a project yet.
- **Sessions.** Signing in sets an `HttpOnly` cookie holding a random token that names a row in `sessions`. A session lasts a year from its last use: `GET /api/session`, which the web app asks on every load, starts the year again and sets the cookie anew, at most once a day. The cookie is marked `Secure` when `WEACON_URL` is an `https` address.
- **Addresses.** `mail::is_address` decides what an email address is, for sign-in and for invites.

## GitHub

Each team has its own private GitHub App, registered and installed by the team's owner from "Team settings". `github.rs` holds every call to GitHub.

- **Registering.** The web app posts an app manifest to GitHub from a form. GitHub sends the owner back to `/settings/team` with a one-time code, and the server trades it at `POST /app-manifests/{code}/conversions` for the app's id, name and private key, saved on the team's row. The web app keeps a random `state` in the browser tab, made for the open team, and registers a code only when it comes back with that state. The route can only be called from the web app in the owner's browser, so the check lives there.
- **Installing.** "Choose repositories" opens the app's install page on GitHub. GitHub lets a private app be installed only on the account that owns it, so an app has at most one installation and it belongs to the team that registered it. The server stores nothing about it.
- **Calling GitHub.** The server signs a short-lived token with the private key, asks for the app's installation, trades for an installation token, and lists the repositories. A project's repository must be in that list. Nothing is kept between requests.
- **Access.** The app may read the code of the chosen repositories and nothing else. It has no webhook, so GitHub never calls the server.
- GitHub's API address is part of the router's state, so the tests point it at a fake GitHub they start themselves.

## Code map

A project tracks chosen branches of its repository, and the server keeps a map of each: every file, the symbols defined in it, and which file uses which. `map.rs` holds the queue, the worker and the limits; the `code-map` crate does the reading and the linking, and knows nothing of GitHub or the database.

- **The queue.** A row of `branches` has `head_sha`, the latest commit GitHub has, and `commit_sha`, the commit its map describes. A branch waits to be mapped while they differ.
- **The worker.** One task, started by `main.rs`, runs a round every minute and at once when a project's branches change. A round asks GitHub for each tracked repository's branches and notes their commits, then maps every waiting branch.
- **Mapping.** The server asks GitHub for the commit's files, which come with git's id of each content. Contents it has already read for the project are skipped. If any is new, it downloads the commit as one archive and reads only the new ones. The branch's file list and commit are saved in one transaction, which does nothing if the branch or the project's repository changed meanwhile, or if a content it counted on was deleted meanwhile with another branch; the branch is then mapped again.
- **By content.** What is read from a file is stored against git's id of its content (`blobs`, `blob_symbols`, `blob_imports`, `blob_mentions`), so a version of a file is read once, across commits and branches. The id is kept together with what of the file's name changes how it is read: its extension, or the whole name of a Vue or Svelte file. The same content under `notes.txt` and `notes.rs` is so read twice. `branch_files` says which contents a branch has. Contents no branch lists are deleted.
- **Limits.** A call to GitHub gives up after 10 seconds without a connection or 60 without data, so one stalled call can't stop the worker. Downloading an archive may take 10 minutes in all. An archive over 200 MB, one that unpacks to more than 2 GB, which is told from one that ends at 2 GB, more than 200 MB of new text, a file list GitHub cut short or an archive that can't be read whole all become the branch's error, and its map stays at the last commit read.
- **Reading.** `code_map::read` parses a file with tree-sitter and runs one query over it, which says what the file defines, the scope it declares (a namespace or a package), what it imports, as written, and the names it mentions. A mention also records whether it is written after a dot, an arrow or a double colon, as a member of something else. The query is the one shipped with the grammar, where there is one, followed by ours in `crates/code-map/queries/`, which adds the scope, the imports and the mentions the shipped one misses. `LANGUAGES` in the crate lists the languages: JavaScript, TypeScript, Vue, Svelte, Rust, Python, PHP, Go, Java, C, C++, C#, Ruby, Swift, Dart, Elixir, Lua, Kotlin, Scala, Bash and Zig. Beside its query a language has three facts: what separates the parts of an import, which file stands for its folder, and its family, the languages whose files import one another. Other files are listed with their line count only. So is a file over 500 KB, one that isn't text, one with a line longer than 5,000 bytes, which is minified or generated, and one in a folder named `vendor` or `node_modules`, which is other people's code. The docs writer isn't given the text of those either. A file's language comes from its name when the map is asked for, since one content can sit under two names.
- **Components.** A React component is used where its tag is written, so `<Card />` in a `.jsx` or `.tsx` file mentions `Card`. Of a Vue or Svelte file only the scripts are parsed, as TypeScript, with the rest blanked so that lines stay where they are, and its comments are blanked first; the file itself is a symbol of the kind `component`, named after the file, and each capitalised tag in it mentions a component. A tag written `<my-card>` is not matched to `MyCard.vue`.
- **`.h` is read as C,** also when it is a C++ header.
- **Uses.** `code_map::link` takes every file of a branch with what was read from it. It is one rule for all languages, worked out when the map is asked for, since it depends on where each file sits:
  - **An import is found** as a full name, a scope plus a definition, or a scope alone, which stands for every file in it; then as a path beside the importing file; then as a path from the repository's root; then as the end of a path, with the import's leading parts dropped one at a time, so `@/lib/api` is the file whose path ends in `lib/api.ts`. One part alone is matched that way only after a word that stands for a folder, such as `crate` or `@`; after `crate`, `@` or `~` the file highest above the importing file wins. A path written with an extension of its own family is tried without it too, so `./price.js` is `price.ts`. A file that defines the very name it imports, as `mod db;` does, is looked for as a path. Where a dot or the like separates an import's parts, its last part may be a name inside what is imported and is dropped for a second try. Of several files that fit, the one whose path follows the name further wins, then the one that shares the longest folder path with the importing file. No settings file such as `composer.json` or `tsconfig.json` is read.
  - **A file uses another** when it imports it; or mentions a name defined by a file in a scope or folder it imports; or mentions a name, not as a member, defined in its own scope, which is its folder in a language without scopes.
  - **One guess is left:** a name that starts with a capital, as the names of types and components do, that one file alone in the same family defines, and that the file doesn't import from outside the repository. Swift needs it, where a file imports nothing from its own module, and Ruby on Rails.
  - Which method of which class a call reaches is not worked out.
- **A new reader.** `READER` in `map.rs` is raised when what the crate reads from a file changes, such as with a new language. At startup a server with another number forgets every map, and the worker reads them all again.
- **The picture.** The web app draws a branch as a graph: a dot is a file, inside the circle of its folder, and a line joins a file to one it uses, bent toward the folder both share so that lines gather into bundles. `d3-hierarchy` packs the circles once, so nothing moves afterwards but the view: the wheel zooms, dragging moves, and clicking a folder fits it. Clicking a file shows its symbols and lights up its lines, solid to what it uses and dashed from what uses it. The layout is worked out in the browser and saved nowhere.
- The server asks GitHub; GitHub never calls it. A webhook would only have to write `head_sha`.

## Generated docs

A project's docs are written by an AI model from the code map: an overview, "How it works" pages that walk through what a user does and gets, and "How it's built" pages on the project's own code. `docs.rs` holds the run and its prompts; `writer.rs` holds the writers.

- **Writers.** The team's owner picks one in "Team settings". To the run they are the same thing, a prompt in and text out. An API key is the team's and is used on the server: the Anthropic API. A subscription is a person's and is used only on that person's machines: Claude Code or Codex, on a machine of whoever starts the run, makes the request or edits the page being checked (see Machines). The default models are cheap ones, `claude-haiku-5-5` and `gpt-5.6-luna`, and the team's owner can type another. The server itself starts no program.
- **Three stages.** First the writer gets the README and an outline of the repository, every file with its first 40 symbols, and answers as JSON with the folders that aren't the project's own code and up to 8 pages of each kind, each with the files it needs. Then each page is written from only its files, sent with line numbers, under the rules of its kind. Last comes the overview, written from the finished pages.
- **Rules of each kind.** "How it works" is numbered steps in the product's own words, with no file or framework named; the server appends the files it was written from. "How it's built" explains how the code flows, then what to handle with care and what looks like a problem, each claim with its `path:line`. The framework is mentioned only where the flow needs it.
- **The run.** `POST .../docs` claims the project, spawns a task and answers at once; the project's `docs_state` and `docs_message` say where the run is. What a run wrote is saved together at the end, as pages or as proposals (see below), so a run that fails changes nothing. Each run has a number kept in `docs_run`, and its progress, failure and pages are saved only while the project still holds it, so a run can't write into a project that was deleted or given another repository meanwhile. Such a run stops at its next progress update, before it asks the writer for anything more. A run reads the map and the files' text in one transaction, so both are of the same commit. Changing a project's repository keeps its pages, since people may have written in them, and drops what tied them to the old repository: their files, commit and notes, the proposals, and the project's branch and commit. A run that crashes is marked `failed`, and so is every project still marked `running` at startup, since a run can't outlive the server.
- **Text.** The map keeps each text file's content beside its symbols (`blobs.text`), so a run downloads nothing and matches the map's commit.
- **Citations.** The web app turns a `path:line` or `path:line-line` in backticks into a link to GitHub at the docs' commit, but only when the map has that file and it is that long.
- **Editing by hand.** A member edits a page where it is read, and adds, deletes and drags pages into another order; `pages.rs` holds the routes. A person's edit is saved at once.
- **Proposals.** Only the first run on a project without docs saves pages. Every later run, and every request, saves rows of `proposals`: a new text for a page, a new page, or a page to remove, each with the writer's reason. A run plans as before and is given the titles that exist; a planned page whose set and title match an existing one is written with that page's text in hand and told to keep it where the code hasn't changed, and a page that would come out the same gets no proposal. A page whose files have all left the repository is proposed for removal, never one a person made. A run drops the proposals nobody answered. Accepting a proposal names the version of its page that the person looked at; if the page has changed since, the server answers 409 and the proposal stays. Otherwise it does what the proposal says and deletes it: taken whole, the text becomes a version by the writer; with changes left out, the mix the web app made becomes a version by that person. The diff is by paragraph, worked out in the browser.
- **What a page was written from** is kept beside its text, never in it: the files the writer read, those it was given and had no room for, and the commit, which the page's citations link into. A run moves the project's own commit, shown under the title, and leaves each page's alone; an accepted proposal brings its own. The docs page lists the files under a page; a public link doesn't have them.
- **Requests.** "Ask" sends one sentence. The writer answers which page it is about, or that a new one is needed, and which files to read; then it writes that page, and the result is one proposal that carries the request. A request holds the project as a run does, so only one of either goes on at a time.
- **Notes.** A page's row keeps the files it was written from. After a person saves an edit, the save answers at once and the writer checks the page behind it, on the branch the docs were last written from or else the project's first mapped branch, against those files, or, for a page a person made, against the files it says the page is about. What it finds is kept on the page as notes: the person's sentence, what the code does instead, the place, and sometimes the sentence corrected. A newer save drops a check still going. A person's words are what differs from the last text the writer gave the page. Proposals carry notes the same way, and the writer is told never to change a person's sentence without one. Notes never leave the team: a public link is a copy of the text alone.
- **Versions.** Every text a page has had is a row of `doc_versions`, with who wrote it, or nobody for the docs writer. The newest is the page, and is also kept on its row in `docs`. A version's id starts with its time, so a page's newest version is its greatest id. An edit names the version it started from; if the page has moved past it, the save answers 409 and the editor shows what saving would change in the newer text, with "Save mine over theirs". History lists a page's versions, newest first, each with what it changed from the one before, worked out by the `diff` package in the browser. It compares any of them with the page as it is, and restores one by saving its text as a new version.
- **The editor** is Milkdown, in `components/DocEditor.tsx`: a page is edited as it reads and saved as Markdown. An image on another host is not fetched there either: its address is made one a browser doesn't load, and put back on saving. Leaving an open editor for another page, tab or proposal asks first. Its toolbar and the menu that "/" opens are UI SDK's, which reaches the editor through an adapter in that file. "Save" first shows what would change, with the `diff` package, and saves on a second press; with nothing typed it saves nothing. The editor is a file of its own that the browser fetches on the first "Edit".
- **Files.** Pasting or dropping a file into the editor, or picking one with its toolbar, uploads it. What its first bytes are decides what it becomes, never what the upload calls itself: a PNG, JPEG, GIF or WebP up to 5 MB is an image, written `![](image:{id})`; anything else up to 20 MB is an attachment, written as a link, `[report.pdf (2.1 MB)](file:{id})`. An image is shown; an attachment is always sent as a download of plain bytes, so that an HTML or SVG file can't run in a reader's browser. The bytes are a file named by the id in a folder beside the database, `weacon.files` for `weacon.db`, readable by the server's account only, with a row in `files` saying which project it belongs to and what a download of it is called. A backup is the database and that folder. A file is served to its team's members, and through a public link only when one of the link's pages names it. A browser may keep a copy but asks before using it, and is answered 304 when it may still have the file, so ending a link or leaving a team ends the file too. Deleting a project removes its files.
- **The list of pages** is sorted by dragging a page's handle, or with Space and the arrows on it. `components/Sortable.tsx` is a small primitive of ours on UI SDK's reorder behavior.
- **Two tabs.** The docs page shows the "How it works" pages as "User docs" and the overview with the "How it's built" pages as "Technical docs". Both tabs are always there, so a page can be added to either. The open tab and page are in the address, as `?tab=technical&page={id}`.
- **Public links.** "Share" on the user docs opens a panel that lists the project's links. "Create link" copies those pages into `shares` and `share_pages`, behind a random token, and `/shared/{token}` shows the copy to anyone, without an account. A link shows the project's name, the day it was shared and the pages; the lines a run adds about a page's files are cut from the copy, so it names nothing of the repository. Editing the docs or changing the project's repository leaves a copy as it was; "Stop sharing" or deleting the project ends the link. The page asks search engines not to list it.
- The docs page renders Markdown with `markdown-to-jsx`. It displays raw HTML as text, restricts link URLs to relative addresses and the allowed protocols in `DocPages.tsx`, which the docs page and the public page share, turns `image:` and `file:` addresses into the project's own, and shows an image on another host as a link, so that opening a page fetches nothing from another host.
- The Anthropic API's address and the two programs' names are part of the router's state, so the tests use a fake API and a script standing in for the programs.

## Machines

A machine is a person's own computer or virtual machine, and only theirs. The core, `weacon-core` in `apps/core/`, runs on it: macOS, Linux, and on Windows inside WSL only. `machines.rs` holds the server's side.

- **The rule.** An API key is the team's and is used on the server. A subscription is a person's and is used only on that person's machines, by the vendor's own unmodified program, which holds the sign-in. Weacon never reads, keeps or passes on a sign-in. This follows what Anthropic and OpenAI allow: a product may not use a person's plan on a server for others, nor offer the vendor's login as its own.
- **Pairing.** "Add a machine" gives a command with a code good once for ten minutes: `weacon-core pair {server} {code}`. The core trades the code, with the computer's name and system, for a token, and keeps the server's address and the token in `~/.config/weacon-core/core.json`, readable by its user only. Which Weacon a core talks to is so the core's own setting. The address must be `https`, unless it is the computer itself (`localhost`), so that the token can't be read on its way, and pairing follows no redirect. The code is used up and the machine saved in one transaction. While pairing it looks for `claude` and `codex` and offers the vendor's installer for one that is missing, running it only on a yes.
- **The connection.** It is always the core that dials: one WebSocket to `/api/machines/connect` with its token, so nothing listens on the machine. It dials again when the connection drops, waiting from a second up to a minute. A core refused with 401 was removed in Weacon: it forgets the token it started with, leaving a newer pairing in the file alone, and stops. The server keeps who is connected in memory (`Cores`), and pings every 30 seconds. Either side ends a connection it has heard nothing on for 75 seconds, as after a laptop slept, and gives connecting or sending one message 20 seconds.
- **One core for each pairing.** A core holds a lock on `core.lock` in its folder, so a second one started there stops at once. When a core connects with a token that is already connected, as a copy of the folder would, the server keeps the newer connection and tells the older `replaced`, which stops for good instead of dialling again.
- **Messages** are JSON, one a frame, with their types in `crates/core-link/`. The core says `agents`: which of the two programs it finds and whether someone is signed in, learned from how `claude auth status` and `codex login status` end. It says so again every five minutes and after a sign-in. It asks the programs on the side, one look at a time and ten seconds each at most, so it goes on hearing the server meanwhile.
- **An ask.** The server sends `ask` with an agent, a model, a system prompt and a prompt; the core runs that agent in an empty temporary folder with only the environment variables in `KEPT`, and says `answer`. Neither agent is given a way to run a command or read a file: `claude` gets no tools and `--safe-mode`, which leaves out the person's own hooks and instructions; `codex` runs with its shell tools disabled and without the person's config and rules, since its read-only sandbox stops writing but not reading. So a repository's text can make the docs wrong but shouldn't be able to make the machine do anything.
- **Work outlives the connection.** The core keeps an answer until the server says `taken`, and says it again after a drop. The server asks again, under the same id, what a core that came back hasn't answered. An answer is taken only from the machine that was asked. An ask fails when its machine has been away for ten minutes, after fifteen minutes in all, or at once when the machine is removed. Both sides keep this in memory, so it doesn't outlive a restart of either.
- **Signing in** happens in a terminal in the web app. The server sends `terminal.open` with an agent; the core starts that vendor's own login, `claude auth login` or `codex login --device-auth`, in a pseudo-terminal, and what it shows and what is typed pass as `terminal.data` between the core and the owner's browser, through `/api/machines/{id}/sign-in/{agent}`. The core picks the command from the agent alone, so nothing sent from Weacon can start anything else there. The server hands the bytes on and keeps none, in the database or the log. It opens that socket only for a page that came from Weacon's own address (`Origin`), since a browser sends the cookie along from any site, and looks at the session again every 30 seconds, so signing out ends it. In the core each sign-in has a number, and one that is closing can't speak or be typed into as the one opened after it; what is typed is written by a thread of the terminal's own. The web app draws the terminal with `ghostty-web`, fetched when a sign-in is opened. The link and the one-time code of a sign-in so pass through the server; whether the vendors count that as going between is theirs to say, and is an open question.
- **In the background.** `weacon-core install` writes a launch agent on macOS or a user service on Linux, with the `PATH` it was run with so that the agents are found, each value written the way that file's format asks, and turns on lingering so that it runs when nobody is logged in. In WSL that needs systemd turned on, which the command explains.
- **A server over SSH.** `weacon-core add user@host`, on a paired machine, asks first, then copies itself to a host of the same system and processor, gets a pairing code with its own token, which is gone if that machine is removed before it is used, and runs `pair` and `install` there. SSH is only how the core gets there.
- A machine is removed with its owner's account.

## Mail

`mail.rs` has one way to send: `mail::send(pool, to, mail)`, where a `Mail` is a subject, some text, an optional code to show large and an optional button that opens an address.

- Each mail reads the saved SMTP settings and connects with them, so there is no connection to keep in step with the settings. With none saved, the mail is printed to the log.

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
| `GET /api/session` | anyone | `{"setup_needed", "user"}`: whether the server has no accounts yet, and who is signed in. Renews the session |
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
| `GET`, `POST /api/teams/{id}/projects/{project_id}/docs` | a member | The project's docs, its proposals and where a run stands; or starts a run from the `branch` given, which writes pages on a project without any and proposals otherwise |
| `POST /api/teams/{id}/projects/{project_id}/docs/requests` | a member | Has the writer answer `{"text", "branch"}` with one proposal; `page` is the page that was open and `earlier` a try that was rejected |
| `POST`, `DELETE /api/teams/{id}/projects/{project_id}/docs/proposals/{proposal_id}` | a member | Accepts a proposal, naming the `version` of its page that was looked at, 409 when the page has changed since, and with `markdown` when only some of its changes are taken; or rejects it |
| `DELETE /api/teams/{id}/projects/{project_id}/docs/pages/{page_id}/notes/{index}` | a member | Takes one of the writer's notes off a page |
| `POST`, `PUT /api/teams/{id}/projects/{project_id}/docs/pages` | a member | Adds an empty page to `works` or `built`, or puts the pages in the order of the ids given |
| `PUT`, `DELETE /api/teams/{id}/projects/{project_id}/docs/pages/{page_id}` | a member | Saves `{"title", "markdown", "version"}` as the page's new version, 409 when the page moved past `version`; or deletes the page and its history |
| `GET /api/teams/{id}/projects/{project_id}/docs/pages/{page_id}/versions` | a member | Every text the page has had, newest first |
| `POST /api/teams/{id}/projects/{project_id}/files?name=` | a member | Keeps the request's body as a file called `name` and answers `{"id", "image"}`, where `image` says whether it is shown or downloaded |
| `GET /api/teams/{id}/projects/{project_id}/files/{file_id}` | a member | The file: an image as it is, anything else as a download |
| `GET /api/shares/{token}/files/{file_id}` | anyone | A file that one of the link's pages names |
| `GET`, `POST /api/teams/{id}/projects/{project_id}/shares` | a member | The project's public links, or makes one from its user docs as they are now |
| `DELETE /api/teams/{id}/projects/{project_id}/shares/{token}` | a member | Ends a public link |
| `GET /api/shares/{token}` | anyone | What a public link shows: `{"title", "created_at", "pages"}` |
| `PUT`, `DELETE /api/teams/{id}/writer` | the team's owner | Saves or forgets who writes the team's docs; a key is never read back |
| `GET /api/machines` | signed in | Your machines, each with its agents and whether its core is connected |
| `POST /api/machines/codes` | signed in, or a machine's token | `{"code", "server"}`: a pairing code for that person, good once for ten minutes, and the address to pair with |
| `POST /api/machines` | anyone with a code | Trades `{"code", "name", "os"}` for `{"token"}` |
| `DELETE /api/machines/{id}` | its owner | Ends the machine and cuts its core off |
| `GET /api/machines/connect` | a machine's token | The core's WebSocket |
| `GET /api/machines/{id}/sign-in/{agent}` | its owner | A WebSocket to the sign-in terminal of `claude` or `codex` there; refused for a page of another site, while the machine is away or the sign-in is open elsewhere |
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

A fresh server opens on "Set up Weacon". After that people register and sign in with emailed codes, each account has its own team, the owner can set up SMTP, and a team's owner can rename it, invite people by email and set up the team's GitHub App, and members make projects, with or without a repository. A project with a repository has a code map of each branch it tracks, and docs generated from it. The shell is still the empty layout from the reference.

Not built yet, each waiting for the first feature that needs it: the WebSocket.
