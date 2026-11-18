# Plan 08: fixes from the review of 18 November

**Status: in progress**

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: fix what an outside review of plans 02 to 07 found and that holds up against the code. Each finding was checked by reading the code it points at; the advisories were checked against the RustSec database and crates.io. Nothing here adds a feature.

The review's findings are almost all true. Where this plan differs from it, the fix is smaller than the one it proposes, or the finding is left out with a reason at the end.

## 1. Dependencies

| Finding | Checked | Fix |
| --- | --- | --- |
| `lettre` 0.11.23 has six advisories | True. All six are fixed in 0.11.24, published 17 November. Two touch us: a quoted address is delivered to another mailbox, and replies around `STARTTLS` can be forged | Update to 0.11.24 |
| Bundled SQLite has an advisory, and no fixed version fits SQLx | The advisory is true. The second half is out of date: SQLx 0.9.1, published 18 November, accepts the fixed SQLite | Update SQLx to 0.9.1 and `libsqlite3-sys` to 0.39 or later |
| `picomatch` in the web build tools | True, with no fixed version yet. It runs only while building, never in what we ship | Nothing now |
| The Markdown renderer swap already in the working tree | It builds, and the web app's lock file drops from 198 packages to 117 | Keep it, look at a docs page in the browser, commit it first and on its own |

## 2. Signing in, invites and secrets

| Finding | Checked | Fix |
| --- | --- | --- |
| Asking for a new sign-in code resets the count of wrong tries | True, and the most serious finding: five guesses, a new code, five more, with no limit, which is enough to guess a six-digit code. Each request also mails the victim | A new code keeps the count of wrong tries while the previous code is still live. A second request for the same address within a minute changes nothing and sends nothing, with the same answer as today |
| A session never expires on the server | True: the cookie lasts a year, the row lasts forever | The lookup ignores a session older than a year. The cookie gets `Secure` when `WEACON_URL` is `https` |
| An address is only checked for an `@` | True. `a@` is accepted without SMTP, and with SMTP it fails halfway through a batch of invites | One function parses an address with `lettre`, used by sign-in and invites. A batch is checked and its duplicates dropped before anything is saved or sent |
| Accepting an invite can answer 200 with no team | True, when the invite is revoked between the check and the use | 404 when the invite is gone |
| A GitHub code is accepted when no registration was started | True: a link to `/settings/team?code=...` with no state passes, because nothing equals nothing. Someone could make a team's owner register a GitHub App that isn't theirs | The browser registers a code only when it holds a state, the state matches, and it was started for the team now open. See "Left out" for why this stays in the browser |
| The request log holds invite tokens | True: the log records the whole address | The log records the route's pattern, such as `/api/invites/{token}` |
| The database files can be read by other accounts on the computer | True: they are created readable by everyone. They hold sessions, the SMTP password, GitHub keys, writer keys and source code | At startup the server makes the database and its two side files readable by its own account only, new or existing. `weacon.db*` goes in `.gitignore` |
| Mail settings live in two places | True: the live connection and the saved row can disagree after a failed save | Sending reads the saved settings each time. The `Mailer` held in the server's state goes away, which is less code than today |

## 3. Docs writers

| Finding | Checked | Fix |
| --- | --- | --- |
| A local writer can read the server's files | True for Codex, and plan 07 was wrong to say otherwise: its read-only sandbox stops writing, not reading, so text in a repository could talk it into copying a file of the server into a page. Not true for Claude Code, which runs with no tools, though it does load the owner's hooks and settings | Codex runs with its shell tools switched off, without the owner's config and rules. Claude Code runs with `--safe-mode`. Both get only the environment variables they need. Checked by hand: each is asked to print a file of the computer and must fail. If Codex can't be made to fail, its choice in "Team settings" says it is for repositories you trust, and the docs say why |
| Generated Markdown loads images from anywhere | True, and it is the way out for anything a writer was talked into: opening the page sends the image's address to its host | An image is shown as a link |
| Odd output from the writer leaves a project "running" for good | True: a `}` before the first `{` crashes the run, and nothing marks a crashed run | The plan is read safely, and any run that crashes is marked failed |
| "Written from" names files that weren't sent | True | It lists only the files sent |
| An old run can write into another project | True, in a narrow case: a project deleted during a run, and SQLite giving its number to the next project made. A repository changed during a run has the same effect | Each run gets a number saved on the project; progress, failure and the final save do nothing unless the project still holds it. See "Left out" for project numbers |
| Docs outlive a change of repository | True | Changing or removing a project's repository deletes its docs |

## 4. The code map

| Finding | Checked | Fix |
| --- | --- | --- |
| One stalled call to GitHub stops mapping for every team | True: the calls have no time limit and there is one worker | A limit on connecting and on waiting for data |
| Mapping has no size limit | True: the archive is held whole in memory, twice, and so is all newly read text | The archive is refused past 200 MB as it downloads, the copy goes away, and a mapping keeps at most 200 MB of new text. Over either, the branch keeps its old map and shows why |
| A partial map can be saved as complete | True in three places: GitHub's "this list is cut short" flag is ignored, a broken archive reads as "the rest is missing", and a crashed reader reads as "nothing new" | Each becomes the branch's error, and the map stays at its last good commit |
| A late mapping can save into a changed project | True, and it mends itself within a minute except when a project's number is reused | The save does nothing unless the branch still waits for that commit and the project still has that repository |
| A file's language sticks to its content | True: the same content under another name keeps the first name's language. The common case is an empty `.gitkeep` and an empty `__init__.py` | The language comes from the path when the map is read. The stored column goes away |
| Reads can mix two versions of a map, or crash | True, in a window of milliseconds while a map is saved | The map is read in one transaction, the project list no longer assumes a branch's project is there, and the graph skips a line with a missing end |
| The reader counts lines from the top for every symbol | True, and slow on a minified file with thousands of symbols | Line starts are found once; defined names go in a set |
| A project and its branches are saved separately; every save wakes the worker; the branch picker signs in twice | All true, all small | One transaction; the worker is woken only when tracking changed; one token |

## 5. Web app and server pages

| Finding | Checked | Fix |
| --- | --- | --- |
| An open project page misses later pushes | True | It refreshes every minute once nothing is waiting |
| A project can be saved before its branches have loaded, tracking none | True | "Save" waits for the list |
| A failed first request leaves a blank page | True | "Weacon can't be reached" with "Try again"; an error that isn't JSON shows its status |
| A missing file under `/assets` answers with the web app's page | True. And `/ws` does too, though `docs/architecture.md` says it answers 404 | Both answer 404 |

## Tests

In `apps/server/tests/api.rs`:

- Wrong codes still count after a new code is asked for, and a second request within a minute sends nothing.
- A session older than a year is refused.
- A batch with a bad address is refused whole, and a repeated address is invited once.
- An unchanged file renamed from `.txt` to `.rs` is shown as Rust.
- Changing a project's repository removes its docs.
- A slow fake writer: the project is deleted, or its repository changed, while it waits; nothing is written and the run's state isn't touched.
- A writer answering `} {` ends as a failed run.
- A file list cut short and a broken archive leave the old map and report the error.
- A missing asset and `/ws` answer 404.

The existing docs test's wait gets a time limit. The GitHub state, the two writers' isolation and the web app's changes are checked by hand.

## Steps

- [x] Commit the Markdown renderer swap, after a look at a docs page
- [x] Dependencies: `lettre`, SQLx and SQLite
- [x] Sign-in codes, session expiry, the address check, invite acceptance
- [x] The GitHub state, the request log, the database files' permissions
- [x] Mail reads its saved settings
- [ ] Writers: isolation, checked by hand with both programs
- [x] Docs: the run's number, safe plan reading, crashed runs, "Written from", images, docs deleted with the repository
- [x] Map: time limits, size limits, no partial maps, the guarded save
- [x] Map: language from the path, one read transaction, the reader's scans, the project save
- [x] Web app and server pages
- [x] Server tests
- [x] Docs: `docs/architecture.md`; plan 07's line on what a program can touch; the left-out findings moved to `improvements.md`

## Outcome

Built as planned, with these differences:

- SQLite went to `libsqlite3-sys` 0.40, the newest SQLx 0.9.1 accepts.
- Codex was checked by hand. Asked to print the first line of `/etc/hosts` and an environment variable, it printed the line as it ran before, and answered that it can't with `--disable shell_tool --disable unified_exec --ignore-user-config --ignore-rules` and a trimmed environment. A real docs run with those options wrote 15 pages: the overview, 7 flows and 7 technical pages.
- Claude Code is the unticked step: `claude` on this computer still isn't signed in outside the desktop app, so `--safe-mode` and the trimmed environment were accepted up to the sign-in check and no further.
- A citation of several lines, `path:10-20`, is now a link too. The first real docs had more of those than of single lines, and none was linked.
- After five wrong tries the code request keeps counting them until ten minutes pass with no new code, so the message for a refused code now also says "or was tried too often".
- The time limits on GitHub calls and the two 200 MB limits have no test: each would need a fake GitHub that stalls or sends 200 MB.
- The guarded map save has no test of its own, since a mapping can't be paused from a test. The two docs tests cover the same guard on the docs side, and were seen to fail with the guard taken out.
- The GitHub state was checked in the browser: opening `/settings/team?code=...` with no state sends nothing to the server.

## Left out on purpose

| From the review | Why |
| --- | --- |
| Keeping the GitHub state on the server | The route can only be called from our own page in the owner's browser, so the missing check is in the browser, and two lines there close it. A table of states would guard nothing more |
| Project numbers that are never reused | SQLite can't add that to a table; it means rebuilding `projects` with everything that points at it. The run's number and the guarded save close the same hole |
| Checking citations against the commit the docs were written from | We keep one map per branch, not one per commit. The page already says when the branch has moved on |
| A test SMTP server, and tests against a database file with several connections | The first tests `lettre`, not us. The second can't be made to fail on purpose |
| Loading less while generating docs, and a limit on runs at once across projects | True and harmless today. Goes to `improvements.md` |
| Foreign keys in place of the map's clean-up statements | It needs four tables rebuilt and every map read again, to remove about ten lines |
| The development script's `kill 0` | True only when something other than a terminal starts the script. The fix the review gives leaves the server running, since `cargo run` doesn't pass a signal on, and macOS's own bash lacks `wait -n` |
| Choosing a file in the graph by keyboard | True. It is a new control, not a fix; goes to `improvements.md` |
| A CI check | True: nothing runs the build and the tests on a push. When you ask |
