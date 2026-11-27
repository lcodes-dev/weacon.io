# Plan 41: cleaning up the architecture

**Status: done**

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: now that every feature is built, the code reads like a well-kept Laravel app: simple, idiomatic Rust and React, names that say what things are, as few comments as possible, and an architecture that a newcomer follows in one sitting. Lorenzo set this on 26 November: review the architecture first, then each area, and go round again up to five times, since cleaning an area can call for a change to the architecture. Background work goes through a queue of jobs, as in Laravel or Rails. Nothing here adds or removes a feature. It ends with a pull request for Lorenzo's review, which this thread does not merge.

## What was found

- **`db.rs` is 3,400 lines,** every query of every area in one file. Finding a table's queries means searching.
- **Each module mixes three things:** the requests it answers, the work behind them and the calls out. `docs.rs` holds its routes, its prompts, the run and the check; `machines.rs` holds its routes, the cores' connection and the asks over it; `github.rs` its routes and the client.
- **Background work is done five ways.** The map has a worker of its own woken by a `Notify`; a docs run is a spawned task with a crash guard and a state on the project; a page's check is a spawned task with a flag on the page; `@weacon` is a task inside a task, with a table of its own and its own clean-up at startup; a chat's turn is a spawned task so that a browser that leaves doesn't cut it short. Each has its own answer to a restart and to a panic.
- **Errors are built by hand:** `ApiError(StatusCode::UNPROCESSABLE_ENTITY, …)` and three helpers in three modules (`teams::refused`, `crate::not_found`, `chats::busy`).
- **Names that need their comment:** `told`, `heard`, `Kept`, `Named`, `Which`, `Written`, `Text`, `Brief`, `own`, `found`, `place`. A comment above most functions says again what the code says.

## The architecture

```
apps/server/src/
  main.rs       reads the settings, opens the database, resumes the queue, starts the schedule and the server
  lib.rs        the areas and AppState
  routes.rs     every route and its handler; auth::team_member guards the team's routes
  error.rs      ApiError and its kinds
  auth.rs       who is asking: a signed-in user, the server's owner, a team's member
  db.rs         opening the pool and making ids
  jobs.rs       the queue
  live.rs       the socket that tells browsers what changed
  accounts/ settings/ projects/ docs/ chats/ machines/ groups/ tasks/
                one folder per area: db.rs its queries, http.rs its handlers, mod.rs and further files the rest
```

- **The queue** (`jobs.rs`) works like Rails' Active Job. A job is a row of `jobs` holding its kind and fields as JSON, its attempts, when it runs next and its last error. `jobs::perform_later` saves the row and runs it at once in a task of its own. A failure that may pass (a 5xx) is retried after `attempts⁴ + 2` seconds, up to three attempts; one that won't (a 4xx) or a panic is discarded; then the job's `failed` step runs and the row goes. At startup a row that was running counts as a failed attempt. Every step is logged with the job's name and id.
- **Jobs:** `MapBranches` and `AnswerAsWeacon` are tried once: the round comes again every minute, and a call may already have made a task. `WriteDocs`, `AnswerDocsRequest`, `CheckPage` and `StartTurn` are tried three times.
- **One database, not two.** A second SQLite file for the queue was considered. In the same file there is one pool, one set of migrations and one backup. Jobs here are a few a minute, so they don't slow the rest down. A second file can come back if that changes.
- **`weacon_calls` goes:** a call is its job, whose unique key is its group, so "working" is whether that job is there.

## Steps

### Cycle 1: architecture

- [x] `error.rs`: `ApiError` with `not_found`, `invalid`, `forbidden`, `conflict`, `unauthorized` and `bad_gateway`
- [x] `db/` in one file per area
- [x] `http/` for the handlers and `routes.rs` for the router; the calls out in files of their own
- [x] The queue, its migration and the six jobs; `map_wake`, `weacon_calls`, `fail_interrupted_docs` and `end_interrupted_weacon_calls` go
- [x] The tests run the queue as the server does, and wait for it instead of calling the map's round

### Cycle 1: areas

- [x] Server: names, comments, repeated code, area by area
- [x] Core: the connection, the terminals and the chats
- [x] Crates: `agent-run`, `chat-copy`, `code-map`, `core-link`, `doc-search`
- [x] Web app: shared hooks, names, comments
- [x] The flaky `a_chat_s_turns_run_on_its_owner_s_machine_and_the_team_reads_them`: it reconnects the core before the server has stored the events it sent

### Cycle 2

- [x] Architecture again: the agent's tools as one method each on a `Tools` struct, the core's terminals in a file of their own, `noUnusedLocals` in the web app
- [x] Areas again: a third pass found only comments that restate the code, which went

### Done

- [x] `docs/architecture.md`, `AGENTS.md` and the README match the code
- [x] `scripts/check.sh` passes
- [x] A pull request for Lorenzo's review

### Cycle 3: Lorenzo's review of the pull request, 26 November

- [x] Every comment goes, but for a dozen on what the code can't show (a sandbox's reason, a security header, GitHub's clock rule)
- [x] The server in one folder per area, with `db.rs`, `http.rs` and `mod.rs`, in place of a file per thing: 69 files become 43
- [x] The queue as Active Job: `perform_later`, `perform`, retries with a polynomial wait, discards on a 4xx, a log line for each step
- [x] Membership checked once, by a middleware on the team's routes, in place of a line in 27 handlers
- [x] Formatted to 110 columns with the widest heuristics, and a value's steps joined where they fit one line: about 2,400 lines fewer
- [x] The web app in one folder per area too, with the small team settings, docs and board files joined into the pages that use them
- [x] The core's setup and service files joined, and doc-search's embedding into its lib

