# Plan 09: queries checked when the server is built

**Status: done** (2026-11-18)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: every query is written with SQLx's `query!`, `query_as!` and `query_scalar!`, so that a misspelt column, a wrong type or a missing value stops the build and not a request. The server stays on SQLite only, and the SQL stays as it is.

The move to SeaORM and Postgres was undone on 18 November and is kept in a git stash, not in the repository.

## How it works

**The macros need to see the schema.** They check each query against a real database while the code is compiled. SQLx offers two ways to give them one:

| Way | What it means |
| --- | --- |
| A folder of saved checks, `.sqlx/`, kept in git | `cargo build` and `cargo test` need no database, as today. After changing a query or a migration, one command writes the folder again |
| `DATABASE_URL` pointing at a database | Nothing to keep in git, but a database with the migrations run must exist before the first build, on every computer |

This plan takes the first. A stale folder can't slip through: a query that isn't in it fails the build.

**One script, `scripts/prepare-queries.sh`.** It makes an empty database in `target/`, runs the migrations on it and runs `cargo sqlx prepare` for the server and its tests. It needs `sqlx-cli` 0.9.1, which mise installs at that version beside Node and Rust.

**`db.rs` keeps its shape:** one function per query, the same SQL, the same names. What changes in each:

| Today | With the macros |
| --- | --- |
| `sqlx::query_as("...").bind(a).bind(b)` | `sqlx::query_as!(Row, "...", a, b)` |
| `#[derive(FromRow)]` on each row struct | Gone: the macro fills the struct by field name |
| Rows read as tuples, such as `(String, String, String, i64, i64)` | Fields with names, checked |
| A user with their teams, a project with its branches: one struct with a skipped field | The macro can't skip a field, so these two are put together by hand from two queries, a few lines each |

All 69 queries in the server and the 8 in its tests are fixed strings, so every one can be checked. None is built at run time.

**Where SQLite needs telling.** SQLite types are loose, so a few columns need a note in the query itself:

- A true-or-false column, and `EXISTS`, come back as a number: written `is_owner as "is_owner: bool"`.
- A value SQLite can't prove is there, such as a `COUNT(*)` or a column through a join: written `name as "name!"`.

How many queries need one is found by converting them; the first step reports it.

## Server code

```
.sqlx/                             new: the saved checks, in git
scripts/prepare-queries.sh         new
mise.toml                          sqlx-cli 0.9.1
apps/server/src/db.rs              every query through a macro; FromRow gone
apps/server/src/lib.rs             the health check's query
apps/server/src/map.rs, writer.rs,
  mail.rs                          FromRow gone where a row struct lives there
apps/server/tests/api.rs           its 8 queries through the macros
```

No new dependency in the server.

## Steps

- [x] `sqlx-cli` in `mise.toml` and `scripts/prepare-queries.sh`
- [x] Trial: signing in and accounts through the macros, with `.sqlx/` written and the tests passing with no `DATABASE_URL`. Stop and report: what the code looks like, how many notes SQLite needed, anything that couldn't be checked
- [x] The remaining queries in `db.rs` and `lib.rs`
- [x] The tests' queries
- [x] Docs: `AGENTS.md` (the rule and the command), `docs/architecture.md`
- [x] Checked by hand: a query broken on purpose stops the build; a fresh clone builds with no database

## What the trial found

Signing in, accounts, members and invites: 23 queries in `db.rs` and 6 in the tests.

- **Every one could be checked,** and the 35 tests pass with no `DATABASE_URL` set.
- **SQLite needed five notes, all the same kind:** `AS "is_owner: bool"` on the one true-or-false column and on four `EXISTS`. No query needed telling that a value is there.
- **A query that is wrong or new stops the build with "run `cargo sqlx prepare`",** not with what is wrong. The script then says it: no such column, and where.
- **`.sqlx/` is one small file per query,** 116 KB for these 29.
- **An older `sqlx-cli` installed by hand comes before mise's.** It worked all the same, since the checking is done by the server's own SQLx, not by the tool.

## Outcome

Built as planned, with these differences:

- **All 77 queries are checked,** 69 in the server and 8 in its tests; none is left as a plain string. The 35 tests pass, and a copy of the repository with no database builds.
- **Two loops over lists of statements became one call per statement,** since a macro takes its SQL written out: the map's clean-up (3) and forgetting every map (5).
- **SQLite needed 19 notes in all:** 5 for true-or-false values, 11 for a value that is always there (such as a repository picked with `repo IS NOT NULL`), and 3 for a column read as one of our own types (the writer, the SMTP security, the port).
- **`db.rs` was 1,164 lines after this plan,** up from 1,102; plan 10 and the review after it brought it to 1,143. `.sqlx/` holds one small file per query.
- **Checked by hand:** a misspelt column stops the build, and the script names it (`no such column: tokn`).

## Left out on purpose

| Left out | Arrives with |
| --- | --- |
| Postgres | The plan for hosting. The macros check against one kind of database, so that plan decides again how queries are written |
| Map saves in batches | When a mapping is seen to be slow. A batch is a query built at run time, which the macros can't check |
| Running the check on every push | A CI check, when you ask |
