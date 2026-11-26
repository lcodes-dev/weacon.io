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
- **Nothing lists a person's sessions or signs them out elsewhere.** A session in use never ends, so a lost or shared computer stays signed in until someone signs out on it. *(noted in plan 13)*

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
- **A rejected request is remembered only by the open page.** Keeping rejected proposals on the server would let the writer see earlier tries after a reload, and let a team see what was turned down. *(noted in plan 15)*
- **A check that fails says nothing to the person.** The page could say that the writer couldn't check it, and offer to try again. *(noted in plan 15)*
- **A link's address in the docs editor is asked with the browser's prompt,** and no toolbar button lights up for the format under the caret. *(noted in plan 16)*
- **Linking a branch is done on every request for its map.** Keeping the result per commit would make it free; it takes about 50 ms for 422 files. *(noted in plan 17)*
- **The code map could say why a line is there:** an import, a name in the same scope, or the guess. *(noted in plan 17)*
- **Leaving the docs editor asks even when nothing was typed.** The editor could tell the page whether it holds changes. *(noted in plan 18)*
- **The archive limit has no test,** since one would need an archive over 2 GB; the limit could be a setting of the reader so that a test can lower it. *(noted in plan 18)*
- **A sign-in started from Weacon opens a browser on the machine too,** when it has a screen, since Claude Code's login does. The core could stop that for a person who isn't at the machine. *(noted in plan 19)*
- **Asks and answers don't outlive a restart** of the core or the server. Chats will need them kept on disk. *(noted in plan 19)*
- **A machine's token is kept as it is,** like a session's. Keeping only a hash would make a copy of the database useless for connecting as a machine. *(noted in plan 19)*
- **A core doesn't say which version it is.** It could, so that the machines page shows it and says when a core is older than the server expects. *(noted in plan 19)*
- **A removed machine's background service starts again every few seconds for good,** each time stopping with "not paired". Being removed could also undo `install`. *(seen checking plan 19 on Linux)*
- **`install` says the service only runs while logged in when lingering was already on** and the system refused to turn it on again. It could look first. *(seen checking plan 19 on Linux)*
- **A command of an agent can read any file its person can,** and since plan 23 can send it to a host its chat allows. Plan 25 tried a limit and was dropped for the code it needs: Claude Code has one setting for it that worked; Codex can't close the home and keep the tools installed in it. Its page has the findings. *(plans 21, 23 and 25)*
- **An agent's program outlives a core that is killed,** until it next prints. The core could end its turns when it is told to stop. *(seen checking plan 21)*
- **A command Codex ran is shown as Codex quoted it for its shell,** so `\n` reads `\\n`. *(seen checking plan 21)*
- **A chat's page asks for news every five seconds when nothing runs,** and every second when something does. A WebSocket would do both better. *(plan 21)*
- **Chats have no model picker:** each program uses its own default. *(plan 21)*
- **A chat's commit isn't signed,** since the core reads only the name and mail from the person's git settings. *(plan 22)*
- **"Commit and push" shows also when nothing new was changed.** The core could say with a turn's end whether the copy holds anything unpushed. *(plan 22)*
- **A pull request's title and body can't be written in Weacon:** the title is the commit's first line. *(plan 22)*
- **The line about an app that only reads has no link** to the app's settings on GitHub. *(plan 22)*
- **A host can only be allowed once a command has wanted it.** The chat's list could take a host typed in ahead. *(plan 23)*
- **An agent needs another turn after a host is allowed,** since its sandbox refuses and isn't asked. Claude Code can ask while its command waits, which plan 23 built and plan 24 took out for simplicity; Codex's `app-server` has such a request too, and nothing documents what turns it on. *(plans 23 and 24)*
- **The system's `curl` fails its certificate check in Claude Code's sandbox on macOS.** *(seen checking plan 23)*
- **A tool that downloads packages and isn't on `agent-run`'s list fails in a chat,** as Cargo, Bundler and pnpm would. Each needs its setting added, or the owner a way to allow a folder by name. *(plan 23)*
- **Every chat downloads its packages anew,** since its cache starts empty. A cache for each project on a machine would be shared by its chats. *(plan 23)*
- **A host a command wanted several times is told several times** in one turn. The chat could show it once. *(seen checking plan 24)*
- **An agent's words can name the chat's cache by its whole path on the machine,** which only the folder of its files is cleaned of. *(seen checking plan 24)*
- **A sign-in of Weacon's own for each agent, in a folder of the core's,** as the project's spec has for each workspace, would leave the person's settings out by itself. Today each program is told flag by flag to ignore them, and Codex can't be run the way that lets it ask, because that way has no such flag. *(seen reviewing plans 21 to 25)*
- **The core takes a chat's hosts from the server as they come.** The spec has a machine keep limits of its own, so that a server gone wrong can't widen what a machine allows. *(seen reviewing plans 21 to 25)*
- **A task's text isn't rendered as Markdown** in its panel, where it is typed. *(plan 27)*
- **An open task's title and text don't follow a teammate's change** until the task is opened again; its other fields do. *(plan 27)*
- **In a narrow window the task's panel leaves the list little room.** The panel could take the whole page there. *(seen checking plan 27)*
- **A project's key is unique only by the request's check,** so two saved at the same instant could share one. *(plan 27)*
- **A turn's token for the tools is kept as it is,** like a session's and a machine's. *(plan 28)*
- **An agent that changes a task with nothing to change still leaves a line in its chat.** *(plan 28)*
- **Claude Code says it has a memory folder of its own for a chat's copy,** empty at the start. Nothing of the person's is in it, but what it writes there stays on the machine. *(seen checking plan 28)*
- **The files under a task's pages aren't links.** The code map could open at a file named in its address. *(plan 29)*
- **A task is always offered three pages,** however far they are from it. A page too unlike could be left out. *(plan 29)*
- **The docs search was tried on one project's docs and twenty questions.** A second project, and one not in English, would say more. *(plan 29)*
- **The first search after a start loads the model while a request waits,** and on a server that has to fetch it, for as long as 30 MB take. *(plan 29)*
- **Deleting a chat asks nothing first,** though what it changed and never pushed is lost. It could say so when there is such work. *(plan 30)*
- **A chat whose machine was removed leaves its folder on that machine,** should its core ever run again. *(plan 30)*
- **The same two people can have several unnamed groups.** "New group" could open the one they have. *(plan 31)*
- **A message in a group can't be changed or deleted,** and nothing tells a person of one outside Weacon. *(plan 31)*
- **A group's page asks again every two seconds.** A WebSocket to browsers would show a message at once, and say who is writing. *(plan 31)*
- **A person can only call their own agent in a group.** The prototype lets one ask for a teammate's, who approves it first. *(plan 32)*
- **A group's agent always works from the project's first mapped branch,** on the first machine that has it. Neither can be chosen. *(plan 32)*
- **Why a tag wasn't followed is shown only to who wrote it,** and is gone after their next message. *(plan 32)*
- **A group's chats are deleted with the group,** and their folders stay on the machines. *(plan 32)*
- **Nothing limits or counts calls to `@weacon`,** which any member makes on the team's key. *(plan 33)*
- **`@weacon` has no model of its own:** it uses the docs' while the API writes them, and the cheap default otherwise. *(plan 33)*
- **The tools `@weacon` used aren't shown in the group,** only its last words. *(plan 33)*
- **An agent's proposals pile up:** a second one for the same page doesn't replace the first, and the list names them alike. *(plan 34)*
- **An agent's proposal gets none of the writer's notes** on a person's words it changes. *(plan 34)*
- **An agent's proposal for a new page names no files,** though a chat's agent knows which it read. *(plan 34)*
- **A teammate reading your chat isn't told when its machine comes or goes,** only you are; their page shows it at the chat's next event. *(plan 35)*
- **Every event of an agent's turn has each open page ask again,** which can be several requests a second where the timer made one. *(plan 35)*
- **A browser whose session ended keeps trying to open the socket** every two seconds, with a red dot, until the page is loaded again. *(plan 35)*
- **The team's page isn't told of changes:** a member who joins or leaves, or a new invite, shows when the page is loaded again. *(seen reviewing plan 35)*
