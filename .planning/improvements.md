# Possible improvements

Things noticed while working that could be better and that nobody has asked for. None is planned. To do one, give it a plan or fold it into the feature that touches it, then remove it from this list.

## Server

- **The fallback test needs a web build.** `unknown_path_serves_the_web_app` reads the real `apps/web/dist/index.html`, so it fails on a checkout where the web app was never built.
- **No clean shutdown.** Ctrl-C kills the server without letting open requests finish.
- **Nothing limits sign-in requests by where they come from.** One address gets a code a minute at most, but someone can still ask for codes for many addresses, and each registration request sends a mail. *(noted in plan 08)*
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
- **A docs run loads the text of every file,** though it needs only the text of the files its pages name. *(noted in plan 08)*
- **Nothing limits docs runs across projects.** Each project runs one at a time, but ten projects can run ten programs at once. *(noted in plan 08)*
- **A link in docs is checked against the branch's map of today,** not of the commit the docs were written from, so after the branch moves a link can point past the end of a file. *(noted in plan 08)*

## Web app

- **A file in the graph can only be chosen with a pointer.** A labeled list of files would let the keyboard and a screen reader open the same panel. *(noted in plan 08)*
- **The status dot starts red.** Until the first health call answers, the dot shows "unreachable" for a moment.
- **The shell is plainer than the reference.** It has no search field, no icons beside Tasks and Docs, and no workspace and account rows at the bottom of the sidebar.

## Setup

- **`npm install` runs on every `scripts/dev.sh` start.** It takes about a second when nothing changed; it could run only when `package-lock.json` is newer than `node_modules`.
- **mise's `lts` alias can be stale.** An old mise resolves `node@lts` to Node 24, so `mise.toml` pins the exact version, which then has to be raised by hand.
- **No formatting or lint check, and no CI.** Nothing runs `cargo fmt`, `cargo clippy`, a TypeScript linter, the web build or the tests on a push.
- **`scripts/dev.sh` stops its whole process group.** Started from a terminal that is only itself; started by another program it can take that program down too. `cargo run` doesn't pass a signal on to the server, so stopping the two by their ids leaves the server running. *(noted in plan 08)*
- **`picomatch` in the web build tools has an advisory with no fixed version** (GHSA-fc5q-39xc-vjpf). It runs only while building. *(noted in plan 08)*
- **`.legacy/` holds about 4 GB of build output.** Its `target/` and `node_modules` folders are ignored by git and can be deleted to free space.

## Docs

- **Plan 01 shows the planned paths.** Its file list says `server/` and `web/`; the code now lives in `apps/`.
