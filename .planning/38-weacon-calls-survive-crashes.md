# Plan 38: `@weacon` calls survive a crash

**Status: done** (2026-11-26)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: a call to `@weacon` always ends with a message in its group, even when the server restarts or the call panics. It takes two items of `improvements.md` from plan 33:

- **A call to `@weacon` is lost when the server restarts,** and nothing says so in the group.
- **A call to `@weacon` that panics leaves its group "working"** until the server restarts.

Today which groups it works in is a set in the server's memory (`AppState::weacon`). A restart empties the set, so the group stops saying "working", but the person who tagged it is never told the call was lost. A panic in the call's task skips the line that takes the group out of the set, so the group says "working" until the next restart, and every new tag is refused as "still working".

## What changes

- **A call is a row,** in a new table `weacon_calls`: the group, who tagged it, and when. The set in memory goes. "Working" and "still working on the last call here" are read from the table, as a chat's are read from `chats.running`.
- **The answer and the end of the call are one transaction:** the answer is posted and the row deleted together, so a group can't show an answer and still be working, or stop working with no answer.
- **A panic ends the call like a failure.** The rounds run in a task of their own, and the task that started them waits for it. A panic there comes back as an error, and the group is told "It stopped on an error in the server. Tag @weacon to ask again."
- **A restart ends the calls it cut short.** At startup, beside `fail_interrupted_docs`, every row left in `weacon_calls` gets an answer in its group, "It stopped when the server restarted. Tag @weacon to ask again.", for who tagged it, and is deleted.

A call cut short is not run again by itself. It may already have made a task or a proposal, and running it again from the start would make them twice. Whoever tagged it reads why and tags it again. The owner chose this on 26 November.

## The table

```sql
-- A call to `@weacon` that hasn't answered yet: one at most per group.
CREATE TABLE weacon_calls (
    group_id TEXT NOT NULL PRIMARY KEY REFERENCES groups (id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
```

Starting a call is `INSERT OR IGNORE`: no row written means a call is already running, which replaces the check on the set and can't race between two tags. A group or person deleted mid-call takes the row with it, and the answer then goes nowhere, as today.

## Where the code goes

| Where | What |
| --- | --- |
| `apps/server/migrations/0021_weacon_calls.sql` | The table |
| `apps/server/src/db.rs` | Starting a call, its answer with its end, the groups it works in, and ending the calls a restart cut short |
| `apps/server/src/weacon.rs` | The row instead of the set; the rounds in a task of their own, a panic told as a failure |
| `apps/server/src/groups.rs` | "Working" read from the table |
| `apps/server/src/lib.rs`, `main.rs`, `tests/api.rs` | `AppState::weacon` removed; the calls ended at startup |
| `docs/architecture.md` | A call kept in the database, and what a restart or a panic leaves in the group |
| `.planning/improvements.md` | The two items removed |

No crate, and nothing in the web app: the answer is an ordinary Weacon message, and "working" reaches the page as it does today.

## Not in this plan

- Running a cut short call again by itself.
- Saying which tools a cut short call had already used, which waits for the item on showing its tools.
- A limit on how long a whole call may take: each request to Anthropic already gives up after fifteen minutes, and a call has at most ten.

## Tests

In `apps/server/tests/api.rs`, with the stand-in for Anthropic plan 33's test uses:

- A call left running when the server restarts: on the same database file, a second server's startup posts "It stopped when the server restarted" in the group as Weacon's, the group isn't working, and a new tag runs.
- Plan 33's tests pass unchanged with the table in place of the set, among them a second tag told the call is still working.

A panic can't be reached through a request, so that path is checked by reading, and by hand with a panic put in a tool for a moment.

## Steps

- [x] Server: the table, a call kept in it, and its answer with its end
- [x] Server: a panic told in the group as a failure
- [x] Server: the calls a restart cut short ended at startup, with their test
- [x] Docs: `docs/architecture.md`, and the two items out of `improvements.md`
- [x] Checked by hand: a panic put in a call for a moment

## Outcome

Built as planned.

- **Tests:** 66 on the server, one of them new: a call left running is answered "It stopped when the server restarted" by a second server on the same database file, and a new tag then runs. One test of a chat on a machine failed once in the full run and passed alone; it doesn't touch `@weacon`.
- **Checked by hand** with a panic put in a call for a moment, in that test: the group was told "It stopped on an error in the server" and stopped working. Not checked in the dev app.
