# Plan 12: fixes from the review of 19 November

**Status: done** (2026-11-19)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: fix what a second outside review, of the four commits since plan 08, found and that holds up against the code. Each finding was checked by reading the code it points at; the first was also reproduced on the dev server. Nothing here adds a feature.

Of 17 findings, 14 are true and worth a fix, 2 are true but the fix proposed would do harm, and 1 is a matter of taste for the owner.

## 1. The two to fix first

| Finding | Checked | Fix |
| --- | --- | --- |
| Editing projects at the same time answers 500 | True, and worse than reported: renaming four projects at once on the dev server failed for three of the four, six times out of six. `update_project` reads, then writes, in one transaction; SQLite refuses the write when another one landed in between, and waiting doesn't help. The map's worker writes all the time, so a single save can hit it too | That transaction starts as a writer, with `BEGIN IMMEDIATE`. It is the only one that reads first; the others begin with a write. One test on a database file, where today's tests use one connection in memory and can't see this |
| Untracking a branch can drop a file from another branch's map | True, in a narrow window: while `main` is being mapped and counts on a content it knows from `release`, untracking `release` deletes that content, and `main`'s map is saved pointing at nothing. The file stays missing until its content changes | The save checks, inside its transaction, that every file's content is there. If one isn't, nothing is saved and the branch is mapped again on the next round |

## 2. The code map

| Finding | Checked | Fix |
| --- | --- | --- |
| Identical files get the first one's symbols | True. What was read is kept by content, so `Settings.vue` identical to `Profile.vue` is called `Profile`, and a file renamed from `.txt` to `.rs` unchanged shows as Rust with no functions. The second was already in `improvements.md` | What was read is kept by content and by how the name makes it read: the file's extension, and for a Vue or Svelte file its name. A few lines in `map.rs`; the reader's number goes up |
| A comment in a Vue or Svelte file counts as code | True: `<!-- <Ghost /> -->` draws a line to `Ghost.svelte`, and a commented-out `<script>` gives symbols | Comments are blanked first, the same way everything outside the scripts already is |
| Nothing limits how much an archive unpacks to | True. An archive is refused past 200 MB, but entries we skip are still unpacked on the way to later ones, so a small archive of huge files keeps the one worker busy for a long time | The unpacked stream stops at 2 GB, which fails the mapping with a reason |
| The time limit on a GitHub call starts again with every byte | True | The archive download gets a limit on its whole time, 10 minutes |

## 3. Signing in, docs and the web app

| Finding | Checked | Fix |
| --- | --- | --- |
| After a sign-in mail fails to send, asking again does nothing for a minute | True: the code was saved before the mail failed, so the retry is taken for a second request and answers as if a mail went out | When sending fails, the saved code stops counting as just asked for, so the next request makes and sends a new one. Wrong tries stay counted |
| A docs run whose project changed keeps calling the writer | True: its progress updates do nothing, but nothing tells the run, and it writes every remaining page at the team's cost. The test waits for exactly that | A progress update says whether the run still holds the project, and the run stops when it doesn't |
| A docs run reads the map and the files' text at two moments | True, narrowly: a mapping saved in between labels new text with the old commit. The start also builds the whole map only to learn whether there is a commit, which `improvements.md` had | The run reads the map and the texts in one transaction, and the start asks only for the branch's commit |
| "Create" is enabled after the branch list fails to load | True: a failed request is no longer "pending", and the project is saved tracking nothing | "Create" waits for a list that has loaded; a failure shows its message |
| A citation with lines that don't exist becomes a link | True: only the last line is checked, and a missing file counts as having line 0 | A link needs the file in the map and 1 ≤ first line ≤ last line ≤ its length |

## 4. Tooling and docs

| Finding | Checked | Fix |
| --- | --- | --- |
| `scripts/prepare-queries.sh` fails on a fresh clone | True: the server won't compile without `apps/web/dist`. My own check of a fresh copy made that folder by hand | The script makes it, as `scripts/dev.sh` does |
| Two stray spaces in the Scala query | True, copied from its source | Removed |
| The docs promise more isolation of a writer than was checked | True in part. Codex was checked by hand; Claude Code still wasn't, as plan 08 says, yet `docs/architecture.md` states both as fact. A program does run as the server's account with its home folder, which it needs to be signed in | The docs say what was checked and what wasn't, and that a program runs as the server's account |

## The tests' ids

The review asked to remove the tests' rewriting of `teams/1` and `projects/1` into ids, since after a delete `projects/1` quietly meant another project. The owner agreed on 19 November. A test's client now keeps the ids the API gave it, its account, its own team and the project a helper made for it, and builds its addresses from them: `ada.team("/invites")`, `ada.project("/map?branch=main")`.

## Left out on purpose

| From the review | Why |
| --- | --- |
| A file listed by GitHub and missing from the archive should fail the mapping | Such files are normal: a repository can mark files to be left out of archives (`export-ignore`, common in PHP packages), and links and submodules are never in one. Failing would leave those repositories unmapped for good. A cut-short archive already fails, since plan 08 |
| Refusing to start when the database file can't be made private | A disk without file permissions, or a mounted folder in a container, would stop the server from starting at all. The warning stays |
| A separate account on the computer for writer programs | Setting up the computer, not code. The docs will say what the boundary is |
| A foreign key from a branch's files to their content | It needs the table rebuilt in a migration; the check in the save does the same with one query |
| `picomatch` | Still no fixed version, and only in build tools |

## Tests

In `apps/server/tests/api.rs`:

- Four projects renamed at once on a database file all succeed.
- Two identical Vue files are each named after themselves; a file renamed from `.txt` to `.rs` unchanged gets its functions.
- A commented-out tag and a commented-out script add nothing to the map.
- A sign-in mail that fails to send is followed by a request that tries to send again.
- A run whose project was deleted asks the writer for nothing more.

The window in the first table's second row can't be opened from a test, since a mapping can't be paused; the check is read and reasoned about. The web app's two fixes are checked in the browser.

## Steps

- [x] `update_project` starts as a writer, with its test
- [x] The map's save checks its contents are there
- [x] What was read is kept by content and name; comments blanked
- [x] The archive's unpacked size and download time
- [x] The sign-in code after a failed mail
- [x] A docs run stops when it no longer holds the project, and reads its sources at one moment
- [x] The web app: "Create" and citations
- [x] The script, the Scala query, the docs' wording on writers
- [x] Tests, and `.sqlx/` written again

## Outcome

Built as planned, with these differences:

- **38 tests pass.** The test of projects edited at once and the one of the failed mail were each seen to fail with their fix taken out.
- **A file's content is kept under git's id and the file's extension,** or the whole name for a Vue or Svelte file. The same content under two extensions is so read and stored twice.
- **A stopped docs run ends with "The project changed",** which nobody sees: the project it would be saved on no longer holds the run.
- **The two docs tests now check that a stopped run asks the writer for nothing more,** where they used to wait for it to ask for everything.
- **The window between a mapping and an untracked branch has no test,** as expected. The check is one query in the save.
- **The archive's 2 GB limit and the download's 10 minutes have no test,** like the limits of plan 08.
- **The web app's two fixes were built and type-checked, not tried in the browser:** a failed branch lookup needs GitHub to fail, and a citation needs docs written by a real writer.
