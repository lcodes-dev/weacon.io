# Possible improvements

Things noticed while working that could be better and that nobody has asked for. None is planned. To do one, give it a plan or fold it into the feature that touches it, then remove it from this list.

## Server

- **The fallback test needs a web build.** `unknown_path_serves_the_web_app` reads the real `apps/web/dist/index.html`, so it fails on a checkout where the web app was never built.
- **Missing asset files get `index.html`.** A request for a file that doesn't exist, such as `/assets/old-hash.js`, answers 200 with the HTML page. A path whose last part has a file extension could answer 404 instead.
- **No clean shutdown.** Ctrl-C kills the server without letting open requests finish.
- **Nothing limits how often a sign-in code can be requested.** Someone could make the server send mail to one address repeatedly. *(noted in plan 02)*
- **The session cookie isn't marked `Secure`.** It should be whenever the server is reached over HTTPS. *(noted in plan 02)*
- **The SMTP password is stored as plain text.** Anyone who can read the database file can read it. It could be encrypted with a key kept outside the database. *(noted in plan 02)*
- **Until SMTP is set, registration and sign-in need the server's log.** Codes are only printed there. A setup form that creates the first owner without a code, or SMTP settings from environment variables, would remove the need. *(noted in plan 02)*
- **A team's GitHub private key is stored as plain text.** Anyone who can read the database file can act as the team's app. It could be encrypted with a key kept outside the database. *(noted in plan 05)*
- **Every GitHub list makes four calls.** The installation token lasts an hour and could be kept for it. *(noted in plan 05)*
- **A project doesn't follow a repository renamed on GitHub.** It stores `owner/name`, not GitHub's id. *(noted in plan 05)*
- **Every map update downloads the commit's whole archive** when any content is new. Fetching only the changed files would be lighter on large repositories. *(noted in plan 06)*
- **A minified file fills the map with symbols.** A two-line bundle in a real repository gave 1,072 of them. *(noted in plan 06)*
- **The map answers with every file and symbol at once.** A very large repository makes that answer large. *(noted in plan 06)*
- **Vue files aren't read.** Its grammar ships no tags query, so their script blocks have no symbols. *(noted in plan 06)*
- **The team's API key for docs is stored as plain text**, like the SMTP password. *(noted in plan 07)*
- **The map keeps the text of lock files and bundles too.** Only files over 500 KB are skipped, so the database carries text no docs page will read. *(noted in plan 07)*
- **A docs run writes its pages one after the other.** Several at once would cut the wait. *(noted in plan 07)*
- **A program started by the server inherits the server's environment.** A server started from inside a Claude Code session passes that session's settings on to `claude`. *(noted in plan 07)*
- **Sessions never expire.** They last until sign-out. *(noted in plan 02)*

## Web app

- **The web app is one file of over 500 KB.** The Markdown renderer pushed it past the size where splitting it would help the first load. *(noted in plan 07)*
- **The status dot starts red.** Until the first health call answers, the dot shows "unreachable" for a moment.
- **The shell is plainer than the reference.** It has no search field, no icons beside Tasks and Docs, and no workspace and account rows at the bottom of the sidebar.

## Setup

- **`npm install` runs on every `scripts/dev.sh` start.** It takes about a second when nothing changed; it could run only when `package-lock.json` is newer than `node_modules`.
- **mise's `lts` alias can be stale.** An old mise resolves `node@lts` to Node 24, so `mise.toml` pins the exact version, which then has to be raised by hand.
- **No formatting or lint check.** Nothing runs `cargo fmt`, `cargo clippy` or a TypeScript linter.
- **`.legacy/` holds about 4 GB of build output.** Its `target/` and `node_modules` folders are ignored by git and can be deleted to free space.

## Docs

- **Plan 01 shows the planned paths.** Its file list says `server/` and `web/`; the code now lives in `apps/`.
