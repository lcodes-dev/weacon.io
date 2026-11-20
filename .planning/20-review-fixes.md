# Plan 20: code review and cleanup of machines

**Status: done** (2026-11-20)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: fix what an outside review of plan 19 found on 20 November. Each finding was checked against the code and holds; three weigh less than the review said and one more. The owner asked for all of them.

## What would hurt in daily use

| Found | Fix |
| --- | --- |
| A connection that died without a word still shows as connected, on both sides. A laptop that slept would stay "connected" and never dial again | Each side ends a connection it has heard nothing on for 75 seconds; the server pings every 30. Connecting and sending have time limits too |
| A core removed in Weacon deletes `core.json` even when that file now holds a newer pairing | A refused core forgets only the token it started with. The file is written whole or not at all |
| Two cores on one pairing take the connection from each other every second, and may both answer one ask | One core at a time for each pairing, by a lock in the core's folder. A core whose connection another took over is told so, and stops |
| A machine removed in the middle of a docs run leaves the run waiting 15 minutes | Removing a machine fails what was asked of it and closes its sign-ins at once |
| A core pairs with, and sends its token to, a server at a plain `http://` address anywhere | `https` only, unless the address is this computer itself. Pairing follows no redirect |

## Smaller

| Found | Fix |
| --- | --- |
| The core stops hearing the server while it asks an agent's program whether someone is signed in | That question is asked on the side, one at a time, for ten seconds at most |
| What is typed into a sign-in is written to its terminal inside the core's message loop | Each terminal has a writer of its own |
| A sign-in that is closing can close or write into the one opened after it | Each sign-in has a number in the core, and only the current one speaks. The server clears a machine's sign-ins only with its current connection |
| The sign-in's socket checks the cookie and not where the page came from, and outlives a sign-out | The page must come from Weacon's own address, and the session is looked at again every 30 seconds |
| An answer is taken from any core that knows the ask's id | Only from the machine that was asked |
| A code a machine asked for outlives that machine | It goes with the machine |
| A second account in the same browser briefly sees the first one's machines | Signing in or out empties what the web app remembered |
| A path with a space or an `&` breaks the background service's file | Values are written the way each file's format asks |
| A pairing code is used up before the machine is saved | Both in one transaction |
| No way to get a fresh pairing code without reloading | "New code" beside the command |

## Tests

In `apps/server/tests/api.rs`: removing a machine fails its run at once; an answer from another machine is ignored; a sign-in from another origin is refused; a machine's code dies with it; a core whose connection was taken over is told. In the core: which addresses may be plain, and how values are written into the two service files. The 75 seconds and the lock are checked by hand.

## Steps

- [x] The core: `https`, its own token only, one core for each pairing
- [x] The core: silence ends a connection, status checks on the side, sign-ins by number
- [x] The core: service files written safely
- [x] The server: removal, silence, origin, answers and codes
- [x] The web app: what it remembers, and a new code
- [x] Docs: `docs/architecture.md`
- [x] Checked by hand

## Outcome

Built as planned, with these differences:

- **The new test found a mistake in the fix itself.** An answer from the wrong machine was ignored, but the ask was dropped with it, so the run failed with "Something went wrong". Fixed before anything else was checked.
- **A core that was taken over stops for good,** with a line saying why. It doesn't wait and dial again, which would only take the connection back.
- **The web app forgets everything on signing in and on signing out,** not the machines alone.
- **Tests:** 51 in the server, one new and two grown; 3 in the core, two new.
- **Checked by hand** on macOS, with a scratch server and scratch folders for the core: a plain `http://` address elsewhere is refused before anything is sent; a second core in one folder stops at once; a copy of the folder takes the connection and the first core stops; with the server frozen the core gave the connection up after 65 seconds and was back a second after the server was; a core removed in Weacon left a newer pairing in its file alone, and one removed with its own token forgot it.
- **Not checked by hand:** the service files with a space in a path, which the core's test covers; the server's own 75 seconds; a sign-in from another site, which the server's test covers; "New code" in the browser beyond the build.
