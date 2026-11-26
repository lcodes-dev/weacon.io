# Plan 41: cleaning up the architecture

**Status: in progress**

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
  main.rs       reads the settings, opens the database, starts the queue, the schedule and the server
  lib.rs        AppState
  routes.rs     every route and its handler, in one table
  error.rs      ApiError and its kinds: not found, invalid, forbidden, conflict
  auth.rs       who is asking: a signed-in user, the server's owner, a team's member or owner
  http/         the handlers, one file per resource, thin: check, call, answer
  db/           every query, one file per area, as sqlx checks them at build time
  jobs/         the queue and one file per job
  …             what the server talks to: github, anthropic, mail, cores, live, search, secrets
```

- **The queue** (`jobs/`) is a table, `jobs`, in the same database: a job is a row holding its kind and its fields as JSON, how many times it was tried and, while it runs, when it started. `jobs::dispatch` saves the row and runs it at once in a task of its own, with the state that dispatched it. A job that fails is tried again after a pause, up to the number of tries its kind allows; then its `failed` step runs and the row goes. A panic counts as a failure. At startup the rows left are jobs a restart cut short: each counts as a failed try and is tried again or failed. A job with a unique key, such as the map's round, is dispatched only while no other with that key waits or runs.
- **Jobs:** `MapBranches` (every minute and when a project's branches change), `WriteDocs`, `AnswerDocsRequest`, `CheckPage`, `AnswerAsWeacon` and `StartTurn`. The five ways above become this one.
- **One database, not two.** A second SQLite file for the queue was considered. With the queue in the same file, dispatching a job and the change that needs it can share a transaction, nothing has to be kept in step between two files, and there is one backup. Jobs here are a few a minute, so they don't slow the rest down. A second file can come back if that changes.
- **`weacon_calls` goes:** a call is its job, whose unique key is its group, so "working" is whether that job is there.

## Steps

### Cycle 1: architecture

- [ ] `error.rs`: `ApiError` with `not_found`, `invalid`, `forbidden`, `conflict`, `unauthorized` and `bad_gateway`
- [ ] `db/` in one file per area
- [ ] `http/` for the handlers and `routes.rs` for the router; the calls out in files of their own
- [ ] The queue, its migration and the six jobs; `map_wake`, `weacon_calls`, `fail_interrupted_docs` and `end_interrupted_weacon_calls` go
- [ ] The tests run the queue as the server does, and wait for it instead of calling the map's round

### Cycle 1: areas

- [ ] Server: names, comments, repeated code, area by area
- [ ] Core: the connection, the terminals and the chats
- [ ] Crates: `agent-run`, `chat-copy`, `code-map`, `core-link`, `doc-search`
- [ ] Web app: shared hooks, names, comments
- [ ] The flaky `a_chat_s_turns_run_on_its_owner_s_machine_and_the_team_reads_them`: it reconnects the core before the server has stored the events it sent

### Later cycles

- [ ] Architecture again, with what the area passes showed
- [ ] Areas again, until a pass finds nothing worth changing

### Done

- [ ] `docs/architecture.md`, `AGENTS.md` and the README match the code
- [ ] `scripts/check.sh` passes
- [ ] A pull request for Lorenzo's review
