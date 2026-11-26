# Plan 06: the code map

**Status: done**

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: a project with a repository has a code map for each branch it tracks, kept up to date by the server without anyone pressing a button, and shown as a visual map people can move around in. Plan 07 feeds the map to the model that writes the docs.

## How it works

**Choosing branches.** When a project gets a repository, the form lists the repository's branches with the default one ticked. The person ticks the ones to track, and can add or remove branches later on the same page. An empty repository has no branches, and the page says so.

**Kept up to date by the server.** Each tracked branch remembers two commits: the latest one GitHub has, and the one its map describes. A branch whose two commits differ is waiting, and that is the whole queue. One background worker runs a round once a minute, and at once when a project's branches change. A round has two halves:

*Noticing.* For each project it asks GitHub for the repository's branches, one call that returns every branch with its latest commit, and writes those down.

*Mapping.* For each waiting branch it:

1. asks GitHub for the commit's list of files, which gives every path with the id git has for its content;
2. sets aside the files whose content it has already read, on any commit or branch of this project;
3. if anything is left, downloads the commit as one archive and reads only those files;
4. saves the branch's new list of files and its commit in one transaction.

So a commit that changes three files costs reading three files, a second branch costs only what differs from the first, and ten pushes inside a minute cost one mapping. Nobody builds or refreshes a map by hand.

**Asking now, a webhook later.** A webhook is not the simpler way today, for four reasons:

- GitHub can only call a public address. The server on your computer has none, so the map would never update while developing, and asking would still be needed there.
- It adds a route, a secret per team and a signature check on every call, with a dependency for the check.
- A missed call is never sent again by itself, so a slow round of asking is needed as a safety net anyway.
- Every team's GitHub App, yours included, would have to be registered again, because a webhook is part of the registration.

What a webhook would give is noticing within a second instead of within a minute, and no calls to GitHub while nothing changes. The queue is built so that it can be added in one place: a webhook would write a branch's latest commit exactly as the noticing half does, and the mapping half wouldn't change. One call per project per minute stays far inside GitHub's limit of 5,000 an hour for a team's app.

**Reading a file.** tree-sitter parses it, and the "tags" query that ships with each language's grammar says which nodes are definitions and which are mentions of a name. Every language goes through the same few lines of our code; adding one is a dependency and a line in a list.

**What the map holds.**

| For | It holds |
| --- | --- |
| Every file | Path, language, line count |
| A file in a known language | Its symbols: name, kind (function, method, class and so on), line range. And the names it mentions |
| A branch | Which files it has, at which content |

A file that isn't text, or is over 500 KB, is listed and not read.

**Which file uses which.** File A uses file B when A mentions a name that only B defines. This is matching by name, not understanding the language, so it misses some links and never invents one between unrelated names. It is what lets the map show how the code hangs together.

**When something fails.** A branch that can't be mapped keeps its previous map, shows GitHub's message, and is tried again on the next round.

## The visual map

A graph of the branch, on the project's page, with a panel beside it for the selected file.

- A dot is a file, colored by language, inside the circle of its folder; folders nest inside their parent's circle.
- A line joins a file to one it uses. Lines bend toward the folder both files share, so they gather into bundles.
- Every file is placed once. Nothing moves afterwards but the view: the wheel zooms around the pointer, dragging moves the picture, clicking a folder fits it, and a trail at the top goes back out.
- Clicking a file selects it: its lines turn bright, solid to what it uses and dashed from what uses it, everything unrelated dims, and the panel lists its symbols, each linking to its line on GitHub.
- Names appear as they fit: a folder's at the top of its circle, a file's inside its dot.
- A menu switches between the tracked branches.

It is drawn as SVG. The circles are packed by `d3-hierarchy`, a small library that does only that.

## Choices

- **Read on the server, through GitHub,** so a map exists before any machine is connected. The previous codebase mapped on the person's machine from a local clone.
- **By content.** What is read is stored against git's id of the content, so each version of a file is read once, across commits and branches, and git's own ids save us any hashing.
- **A small relational shape:** files, symbols and mentions, with a `path:line` on everything.
- **Left out:** a row per call site, type inference, graph metrics, clusters, health scores. The previous codebase had call graphs, clusters, flows and pins in about 11,000 lines, for Rust only.

## Database

`apps/server/migrations/0004_code_map.sql`:

| Table | Columns | Notes |
| --- | --- | --- |
| `branches` | `project_id`, `name`, `head_sha`, `commit_sha`, `error` | A tracked branch. `head_sha` is the latest commit GitHub has; `commit_sha` is the one its map describes, empty until first mapped. The branch is waiting while they differ |
| `branch_files` | `project_id`, `branch`, `path`, `blob` | The files of a branch; `blob` is git's id of the content |
| `blobs` | `project_id`, `blob`, `language`, `lines` | One row per content ever read for the project |
| `blob_symbols` | `project_id`, `blob`, `name`, `kind`, `line`, `end_line` | Its definitions |
| `blob_mentions` | `project_id`, `blob`, `name` | The names it mentions |

Contents that no branch lists any more are deleted when a branch is updated or removed.

## API

| Route | Who | Answer |
| --- | --- | --- |
| `GET /api/teams/{id}/github/branches?repo=owner/name` | a member | The repository's branches, and which is the default |
| `POST`, `PUT /api/teams/{id}/projects...` | a member | As today, plus `branches`: the names to track |
| `GET /api/teams/{id}/projects` | a member | Each project also lists its branches with their two commits and error |
| `GET /api/teams/{id}/projects/{project_id}/map?branch=main` | a member | The commit, every file with its symbols, and which file uses which |

## Server code

```
apps/server/migrations/0004_code_map.sql  new
apps/server/src/map.rs                    new: the worker, reading a file, the map route
apps/server/src/github.rs                 three more calls: a repository's branches, a commit's files, the archive
apps/server/src/projects.rs               a project's tracked branches
apps/server/src/db.rs                     their queries
apps/server/src/lib.rs, main.rs           the new routes; main starts the worker
```

New dependencies: `tree-sitter` and `tree-sitter-tags`; one grammar per language; `flate2` and `tar` to unpack the archive. In the web app, `d3-hierarchy`.

**Languages to start with:** TypeScript and JavaScript (with TSX and JSX), Rust and Python.

**Tests**, in `apps/server/tests/api.rs`. The fake GitHub serves branches, file lists and small archives the tests pack themselves, and a test runs one round of the worker by calling it.

- Tracking a branch maps it: every file is listed, with a function and a type from a file in each language.
- A file in an unknown language is listed without symbols.
- A new commit makes the branch wait, then reads only the files whose content changed, and the map shows the new commit.
- A branch that hasn't moved is not mapped again.
- A second branch reads only what differs, and each branch shows its own files.
- A file that mentions a name defined in one other file uses it; a name defined in two files links nothing.
- Removing a branch removes its map and the contents nothing else lists.
- A failing branch keeps its old map and reports the error.

## Steps

- [x] Migration, and the queries in `db.rs`
- [x] `github.rs`: a repository's branches, a commit's files, the archive
- [x] `map.rs`: reading a file's symbols and mentions
- [x] `map.rs`: one round of the worker, and starting it
- [x] Projects track branches; the map route
- [x] Server tests
- [x] Web app: choosing branches on the project's page
- [x] Web app: the visual map
- [x] Docs: `docs/architecture.md`
- [x] Checked in the browser, against a real repository of yours: track two branches, push a commit, watch the map follow

## Outcome

Built as planned, with these differences:

- PHP is read too, because the first real repository mapped was mostly PHP.
- A `READER` number in `map.rs`, kept in `config`, makes a server with a newer reader forget every map and read them again. Without it, files read before a language was added would never get their symbols.
- A file the map doesn't read is drawn small however long it is; otherwise lock files fill the picture.
- Creating a project answers with its id only.
- The picture changed twice in review on 2026-11-17. It was first nested boxes sized by length, then a graph laid out by forces, which rearranged itself on every click. It is now circles packed by folder, zoomable, described above. The boxes and the force layout are removed.
- Checked against `lorenzocattaneo/contentwrite`: 422 files, 1,760 symbols and 133 links between files, mapped a few seconds after ticking `main`.
- Checked again on 2026-11-26 with three branches of `contentwrite`. A new branch was mapped at its own commit six seconds after ticking it. A push to it adding a PHP file that uses `WriteText` showed on the open page 12 seconds later without a reload; the map listed the file, its class and function, and its link to `WriteText.php`, and `main`'s map stayed without it.

## Left out on purpose

| Left out | Arrives with |
| --- | --- |
| A webhook, so GitHub says the moment a branch moves | A server with a public address; it writes to the same queue |
| Downloading only the changed files, not the commit's archive | When an archive per commit costs too much |
| Links that understand the language: imports, types, call sites | When matching by name proves too coarse |
| Arrowheads, and sizing a file by how much it is used | When you ask |
| Search across the map | The plan for search |
| Tools that let an agent query the map | The plan for agents |
| More languages | When you ask; each is one dependency |
