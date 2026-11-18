# Plan 10: keys, indexes and fewer queries

**Status: done** (2026-11-18)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: every table has a fitting primary key, a UUIDv7 where the row has an id of its own; every query finds its rows through an index; and no statement runs that a key could do for us. Nothing here changes what the product does.

It follows an audit of the 69 queries: each has a caller, and SQLite was asked how it runs each one.

## 1. Primary keys

**A UUIDv7 where a row has its own id.** Five tables have a numbered id today:

| Table | Today | Becomes |
| --- | --- | --- |
| `users`, `teams`, `projects`, `invites`, `docs` | `id INTEGER PRIMARY KEY`, counted up by SQLite | `id TEXT PRIMARY KEY`, a UUIDv7 made by the server |

A UUIDv7 starts with the time it was made, so ids still sort by age, and new rows land at the end of the index. It also closes what plan 08 left out: a deleted project's id is never given to another.

**A natural key where there is one.** These tables are found by something that is already unique, and a UUID beside it would be a second key nothing uses:

| Table | Key |
| --- | --- |
| `sessions` | the token |
| `sign_in_codes` | the email |
| `team_members` | team and user |
| `branches` | project and name |
| `branch_files` | project, branch and path |
| `blobs` | project and git's id of the content |
| `blob_mentions` | project, content and name |
| `config` | one row |

**`blob_symbols` has no key today** and gets one: project, content, name, kind and line. Its separate index then goes, since the key starts with the same columns.

**Small tables keyed by text become `WITHOUT ROWID`.** SQLite otherwise keeps such a table twice: the rows, and a second tree from the key to the row. This applies to `sessions`, `team_members`, `branches`, `branch_files`, `blob_symbols` and `blob_mentions`. Tables with long text (`blobs`, `docs`, `teams`) stay as they are, as SQLite's own advice says.

**What a UUID changes outside the database.** Ids in the API and in addresses become text: `/teams/1/projects/3` becomes `/teams/0192…/projects/0192…`. The server's routes take text, the web app's types go from `number` to `string`, and the tests read ids from answers where they now write `1`. An id that isn't a UUID finds nothing and answers 404.

## 2. Indexes

SQLite scans a whole table for these today:

| Query | When it runs | Index added |
| --- | --- | --- |
| A user's teams | Every signed-in request | `team_members (user_id)` |
| A team's projects, and their branches | The project list | `projects (team_id)` |
| A project's docs pages, and deleting them | A docs page; deleting a project | `docs (project_id, position)` |

Every other query already goes through a key. Two scans stay on purpose: the map's worker reads every tracked branch once a minute because it needs them all, and startup looks once for docs runs cut short.

## 3. Fewer queries

No query is unused. These run more statements than they need:

| Today | Becomes |
| --- | --- |
| Untracking a branch deletes the branch, then its files | Files point at their branch and go with it: 1 statement |
| The map's clean-up deletes unlisted symbols, mentions and contents: 3 statements | Symbols and mentions point at their content and go with it: 1 |
| Forgetting every map: 5 statements | 3 |
| The worker writes every branch's latest commit once a minute, changed or not | The write happens only when the commit or the error changed |

## Migrations

The six migration files become one, `0001_schema.sql`, holding the schema as it now stands. An existing database is started anew, as the owner agreed on 18 November: giving every row a new id and rewriting every reference would be a migration in Rust, for databases that hold only test data. A UUID is stored as text, also agreed.

## Server code

```
apps/server/migrations/            one file in place of six
apps/server/Cargo.toml             uuid, with v7
apps/server/src/db.rs              ids as text; the statements above removed
apps/server/src/*.rs               routes take text ids
apps/server/tests/api.rs           ids read from answers
apps/web/src/                      ids typed as string
.sqlx/                             written again
```

One new dependency, `uuid`.

## Steps

- [x] One migration file with the new keys, links and indexes
- [x] `db.rs`: UUIDv7 ids, and the statements the links replace
- [x] The worker's write only on a change
- [x] Routes and row types with text ids
- [x] The web app's types
- [x] The tests
- [x] Docs: `docs/architecture.md`
- [x] Checked by hand: SQLite's plan for each query in section 2 uses its index; the dev server on a new database, with a project mapped

## Outcome

Built as planned, with these differences:

- **Ids are plain text in the server too,** not a UUID type: a query then needs no note, and an id that isn't a UUID simply finds nothing and answers 404.
- **The tests still write `teams/1` and `projects/1`.** Their client turns that into the id of the first team or project made, which works because UUIDv7 ids sort by age. Rewriting 93 addresses to carry ids around would have buried what each test says.
- **The test for a reused project id became a test that a deleted project's run writes nothing,** since an id is no longer reused.
- **Checked by hand:** SQLite's plan for each query in section 2 now searches its index. The dev server on a new database mapped `lorenzocattaneo/contentwrite` to the same 422 files, 1,364 symbols and 133 links as before, the project page opened by its UUID, and deleting the project emptied every map table.
- **The old dev database is kept** in `.dev/before-uuid/`, outside git.

## Left out on purpose

| Left out | Arrives with |
| --- | --- |
| Map saves in batches | When a mapping is seen to be slow; see plan 09 |
| One query for a user and their teams | When the two are seen to cost something: joined, the row has to be taken apart by hand |
| Keeping "which file uses which" with the map | Already in `improvements.md` |
| Moving existing rows to UUIDs | A database someone needs to keep |
