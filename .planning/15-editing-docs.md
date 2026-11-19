# Plan 15: editing docs, by hand and through the agent

**Status: planned**

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: docs stop being something a run replaces whole. People edit pages and add images; the agent's changes arrive as proposals a person approves from a diff; and the agent says where what a person wrote disagrees with the code.

This plan is a design to agree on. It is built in four parts, each usable on its own, and each part's steps are ticked here.

## The rule everything follows

**A person's edit is saved at once. Anything the agent writes is a proposal, and changes nothing until a person accepts it.**

The one exception is the first run on a project with no docs, which saves its pages directly, as today.

## What is kept

| Kept | What it is |
| --- | --- |
| A page | As today, plus the files it was written from, so the agent can read them again |
| A page's versions | Every text the page has had, with who wrote it (a person, or the agent) and when. The newest is the page. This gives history, "restore", and tells the agent which words are a person's |
| A proposal | A text the agent suggests for one page, or a new page, or removing a page; why; and its notes (see part 4). It names the version it was written against |
| An image | A file a person uploaded to the project |

A public link from plan 14 is a copy, so none of this changes what a link shows.

## Part 1: editing by hand, and images

- **"Edit" on a page** opens its Markdown in a text box beside a preview. "Save" makes a new version. A member can also add a page to either tab, rename one, reorder and delete.
- **History** lists a page's versions; any two can be compared, and an older one restored, which makes a new version.
- **Images.** Pasting or dropping an image into the text box uploads it and inserts `![](image:{id})`. PNG, JPEG, GIF and WebP up to 5 MB; no SVG, which can carry scripts. Images are kept in the database, so a backup is still one file, and are served to the team's members only. A public link serves the images its own pages name, and no others.
- **An image on another host stays a link,** as today.
- **Two people editing one page:** a save names the version it started from. If the page moved on meanwhile, the save is refused and the editor shows both texts.

## Part 2: runs propose, they don't replace

"Generate again" becomes "Update docs". The run plans pages as today, then for each one:

| The run finds | It makes |
| --- | --- |
| A page it planned that exists | A proposal with the new text, written with the current text in hand and told to keep it where the code hasn't changed. No proposal if nothing would change |
| A page it planned that doesn't exist | A proposal for a new page |
| A page whose files are gone from the repository | A proposal to remove it, never for a page a person made |

- **Reviewing.** The docs page shows "3 proposals". Each opens as a diff against the page as it is now: what would go in red, what would come in green, by paragraph. Under it, the agent's reason in a sentence.
- **Accepting.** Each changed block has its own accept and reject, with "Accept all" and "Reject" for the proposal. What is accepted becomes a new version written by the agent; a person can edit the result first.
- **A proposal written against an older version** of its page still shows its diff against today's text, with a warning, since accepting a block may undo a later edit.
- **A new run drops the proposals of the one before it** that nobody answered.
- The diff is worked out in the browser by the `diff` package.

## Part 3: asking the agent for a change

A box on the docs page: "Ask for a change", such as "add a page on exporting reports" or, on an open page, "explain the retry step better".

1. The agent gets the request, the list of pages, the open page if any, and the repository's outline, and answers which page to change or add and which files it needs.
2. It writes that page from those files, and the result is a proposal like any other, carrying the request it answers.

- **A request is one message and one proposal, not a conversation.** To push back, a person rejects and asks again, and the earlier request and its proposal are given to the agent as context. A real back-and-forth comes with chats, which Weacon doesn't have yet.
- **One request or run at a time per project,** as runs are today.

## Part 4: where a person's words disagree with the code

Whenever the agent reads a page that holds text a person wrote (in a run, a request, or "Check against the code" on a page), it also answers with notes:

- **A note** quotes the person's sentence, says what the code does instead, and gives the place as `path:line`.
- **Notes show in the diff,** beside the block they are about, and on the page itself after a check with no proposal.
- **The agent never changes a person's sentence silently.** Where it believes one is wrong it leaves it and writes a note, or proposes the change as its own block with the note as its reason.
- **A note is the agent's reading, not a verdict.** A person dismisses it, or accepts the change it suggests.
- **"Check against the code" also runs alone,** after a manual edit, when someone asks; it is not run on every save, since each check costs a call to the writer.
- **On user docs** the note still cites code, though the page itself names none; notes are only ever shown to members, never on a public link.

## Left out on purpose

| Left out | Why |
| --- | --- |
| An editor that hides the Markdown | A large dependency for a first version; the preview shows the result |
| Comments and suggestions from people on each other's edits | Members edit directly; history shows who changed what |
| Editing through a public link | A link is read-only |
| Checking every page on every push | Cost. A page shows the commit it was last checked against, and "Update docs" covers it |

## Steps

Part 1:

- [ ] Versions: migration, queries, and saving a page as a new version
- [ ] Editing, adding, renaming, reordering and deleting pages in the web app
- [ ] History, comparing and restoring
- [ ] Images: upload, serving, and in public links

Part 2:

- [ ] Proposals: migration and queries; the run makes proposals when docs exist
- [ ] The diff, with accepting and rejecting by block

Part 3:

- [ ] A request picks its page and files, and writes a proposal

Part 4:

- [ ] Notes in a run and a request, shown in the diff
- [ ] "Check against the code" on a page

Each part ends with its tests in `apps/server/tests/api.rs`, `docs/architecture.md`, and a check by hand in the browser.
