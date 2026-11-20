# Plan 18: what the review of 20 November found

**Status: done** (2026-11-20)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: fix what an outside review of plans 15 to 17 found, and do the two simplifications it named. Every finding was checked against the code and holds. The owner asked for all of them on 20 November.

## Text that could be lost

| Found | Fix |
| --- | --- |
| Accepting a proposal from a view that is out of date deletes or overwrites an edit made since. Only the browser warned, and only when it knew | Accepting names the page's version the reviewer saw. The server compares it inside the transaction and answers 409, keeping the proposal |
| Changing a project's repository deletes every page, also those written by hand, with their history | The pages stay. What tied them to the old repository goes: their files, notes, proposals, and the project's branch and commit |
| Opening another page or tab while editing drops what was typed | Leaving an open editor asks first |
| A paragraph that starts "Written from:" cuts a public copy short, and what the writer is shown | The files a page was written from, and those left unread, are kept beside the text and no longer in it. Nothing is cut from a text any more |

## Docs that could mislead or leak

| Found | Fix |
| --- | --- |
| A run moves the docs' commit before anyone accepts a proposal, so citations in pages nobody changed point at the new commit | Each page keeps the commit its text was written from. A proposal carries its own and hands it on when accepted |
| The editor loads images from other hosts, which the read view never does | An image on another host is not loaded in the editor either; its address comes back unchanged on save |
| A file stays in a browser's cache for a year, also after "Stop sharing" | A file is checked with the server before a cached copy is used |
| A page written by hand before the first run is never checked | The check falls back on the project's first mapped branch |
| `weacon.files/` beside a default database can be committed by mistake | Ignored, as the database is |

## The code map

| Found | Fix |
| --- | --- |
| `import "./price.js"` doesn't find `price.ts` | A path written with an extension of its own family of languages is tried without it too |
| `crate::price` finds a nearer nested `price.rs` | After `crate`, `@` or `~`, the file highest above the importing file wins, not the nearest |
| The limit on how much an archive may unpack to can pass for the archive's end, and the files after it are then kept as unread for good | Reaching the limit is an error, so the last whole map stays |

## Simpler

- **A docs run and a page's check no longer work out which file uses which.** They need the outline and the text only.
- **The docs pages load when first opened,** so the sign-in and project screens don't carry them.

## Data

One migration: `docs` and `proposals` each gain `commit_sha` and `unread`. A page's commit starts as its project's. The two lines a run used to add under a page are taken off existing texts where they are the text's last line.

## Tests

In `apps/server/tests/api.rs`: a stale acceptance is refused and the proposal stays; pages survive a change of repository; a page keeps its commit through a run nobody accepts; a text with "Written from:" in it is shared whole; a hand-written page is checked before any run; a file is answered 304 when the browser has it. In the crate: the two import cases.

## Steps

- [x] Stale proposals refused; pages kept when the repository changes
- [x] A commit per page; files and unread files beside the text
- [x] The check before a first run; files revalidated; the archive's limit an error
- [x] The code map's two import cases
- [x] The web app: leaving the editor, outside images, the new fields
- [x] Docs runs without linking; docs pages loaded on demand
- [x] Docs: `docs/architecture.md`
- [x] Checked by hand in the browser

## Outcome

Built as planned, with these differences:

- **Leaving an open editor always asks,** whether or not anything was typed, since the docs page doesn't know what the editor holds. "Cancel" in the editor still leaves at once.
- **Changing the repository keeps every page,** generated ones too, since nothing says for sure which of them a person has since written in. A run on the new repository then proposes changes to them.
- **A request's answer may leave fields out or give them as null.** The owner's own run with Claude Code failed on `"section": null` for a page that exists; found on the dev server while checking this plan, and fixed with a test.
- **A module a file declares is looked for as a path.** `mod price;` in two files had each find the other's declaration; the test for the crate's root showed it.
- **Which file uses which is its own query now,** made only for the map's own page. A docs run, a request and a check read the files and their text and nothing more.
- **The main bundle went from 663 kB to 338 kB** by loading the docs page and the public page when first opened. The editor stays a file of its own, 517 kB.
- **Tests:** 47 in the server, one new and six grown; 19 in the crate, two new.
- **Checked by hand** on the dev server, on a throwaway page put into its database: an image on another host is not fetched when the page is edited and comes back unchanged in the diff; opening another page while editing asks first and "Cancel" stays; the owner's generated pages lost their last line of files in the migration and show it under the page instead.
- **Not tried in the browser:** a stale acceptance, which the test covers, and the archive's limit, which would need an archive over 2 GB and has no test.
