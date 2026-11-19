# Plan 15: editing docs, by hand and through the agent

**Status: done** (2026-11-19)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: docs stop being something a run replaces whole. People edit pages and add images; the agent's changes arrive as proposals a person approves from a diff; and the agent says where what a person wrote disagrees with the code.

This plan is a design to agree on. It is built in four parts, each usable on its own, and each part's steps are ticked here. The owner answered its five questions on 19 November, so nothing in it is open.

## The rule everything follows

**A person's edit is saved at once. Anything the agent writes is a proposal, and changes nothing until a person accepts it.**

The one exception is the first run on a project with no docs, which saves its pages directly, as today.

## What is kept

| Kept | What it is |
| --- | --- |
| A page | As today, plus the files it was written from, so the agent can read them again |
| A page's versions | Every text the page has had, with who wrote it (a person, or the agent) and when. The newest is the page. This gives history, "restore", and tells the agent which words are a person's |
| A proposal | A text the agent suggests for one page, or a new page, or removing a page; why; and its notes (see part 4). It names the version it was written against |
| An image | A file a person uploaded to the project, kept in a folder beside the database, with a row that says whose it is |

A public link from plan 14 is a copy, so none of this changes what a link shows.

## Part 1: editing by hand, and images

- **"Edit" on a page** opens its Markdown in a text box beside a preview. "Save" makes a new version. A member can also add a page to either tab, rename one, reorder and delete.
- **History** lists a page's versions; any two can be compared, and an older one restored, which makes a new version.
- **Images.** Pasting or dropping an image into the text box uploads it and inserts `![](image:{id})`. PNG, JPEG, GIF and WebP up to 5 MB; no SVG, which can carry scripts. Images are kept as files in a folder beside the database, named by their id, so a backup is the database and that folder. They are served to the team's members only, and deleted with their project. A public link serves the images its own pages name, and no others.
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

The agent checks a page against the code every time a person saves an edit to it, and every time it writes a proposal for a page that holds text a person wrote, in a request or a run. It answers with notes:

- **A note** quotes the person's sentence, says what the code does instead, and gives the place as `path:line`.
- **Notes show in the diff,** beside the block they are about, and on the page itself after a person's edit.
- **The agent never changes a person's sentence silently.** Where it believes one is wrong it leaves it and writes a note, or proposes the change as its own block with the note as its reason.
- **A note is the agent's reading, not a verdict.** A person dismisses it, or accepts the change it suggests.
- **A save doesn't wait for its check.** The edit is saved at once and the check runs behind it, as one call to the writer with the page and the files it was written from; the page says "Checking against the code" until the notes arrive. A new save drops a check still running for the same page.
- **A page a person made has no files yet,** so its first check starts by asking the agent which files it is about.
- **On user docs** the note still cites code, though the page itself names none; notes are only ever shown to members, never on a public link.

## Left out on purpose

| Left out | Why |
| --- | --- |
| An editor that hides the Markdown | A large dependency for a first version; the preview shows the result. Plan 16 added one after all |
| Comments and suggestions from people on each other's edits | Members edit directly; history shows who changed what |
| Editing through a public link | A link is read-only |
| Checking every page on every push | Cost. A page shows the commit it was last checked against, and "Update docs" covers it |
| A button to check a page without editing it | Checks come with edits and requests; "Update docs" covers a page nobody touched |

## Steps

Part 1:

- [x] Versions: migration, queries, and saving a page as a new version
- [x] Editing, adding, renaming, reordering and deleting pages in the web app
- [x] History, comparing and restoring
- [x] Images: upload, serving, and in public links

Part 2:

- [x] Proposals: migration and queries; the run makes proposals when docs exist
- [x] The diff, with accepting and rejecting by block

Part 3:

- [x] A request picks its page and files, and writes a proposal

Part 4:

- [x] Notes in a run and a request, shown in the diff
- [x] The check after a person's save, and its notes on the page

Each part ends with its tests in `apps/server/tests/api.rs`, `docs/architecture.md`, and a check by hand in the browser.

## Outcome of part 1

Built as designed, with these differences:

- **The page's text is kept twice,** on its row in `docs` and as its newest version, so that every query that reads docs stayed as it was.
- **Both tabs are always shown,** also on a project with no docs, so that a page can be added by hand to either.
- **A new page is named "New page" and opens in the editor,** where its title is changed; there is no separate renaming.
- **Reordering is "Move up" and "Move down"** on the open page, within its tab.
- **"Generate again" still replaces every page,** edits and history included, until part 2. It now asks first.
- **The image's type is told by its first bytes,** not by what the upload says it is.
- **43 tests pass,** three of them new: editing with its history and a refused stale save; adding, ordering and deleting pages; and images, with what a stranger, a signed-out visitor and a public link can and can't fetch.
- **Checked by hand** on the dev server: editing, a pasted image showing in the preview and on the page, history with its diff, restoring, adding, moving and deleting a page. The paste was made by a script, since a real clipboard can't be driven from here.
- **Not tried in the browser:** the view for two people editing one page, and an image on a public page. The tests cover the refused save and the image's route.
- **The web app's bundle passed 500 kB,** which the build now warns about.

## Outcome of parts 2 to 4

Built as designed, with these differences:

- **A planned page is the same page as an existing one when its set and title match.** The plan is given the titles that exist and told to keep them. A page the writer renames comes as a new page, with no proposal to remove the old one unless its files are gone.
- **The writer's answer is the page, then its reason and its notes** after two marker lines, `---REASON---` and `---NOTES---`. Notes that can't be read count as none.
- **A person's words are what differs from the writer's last text of the page,** which the writer is given beside the page as it is.
- **Accepting every change saves the proposal's text as the writer's.** Leaving a change out saves the mix under the name of the person who made it. Editing the result is done with "Edit" after accepting.
- **A change is a run of paragraphs:** the diff splits both texts at blank lines.
- **Notes sit under the change that holds the sentence they quote,** and under the diff when none does.
- **"Generate again" is now "Update docs",** and asks nothing first, since it replaces nothing.
- **The overview gets a proposal like any page,** written from the pages as they would be if every proposal were accepted.
- **A run's branch and commit are saved when the run ends,** before its proposals are answered, so a citation in a page not yet updated may point a few lines off.
- **A request's earlier try** is kept by the web app only while the page is open: rejecting a request's proposal and asking again sends both along.
- **A request about the overview is written by the rules of a technical page.**
- **A check that fails leaves the page without notes** and says so only in the server's log.
- **A note's `instead`** is the sentence corrected, when the writer offers one; "Use the suggestion" puts it in the page as an edit of the person's, which is checked again.
- **46 tests pass,** three of them new: a run over existing docs with accepting whole, accepting a mix, rejecting and a removal; a request; and the check after an edit, on a written page and on one a person made.
- **Checked by hand** on the dev server, with proposals and a note put into its database by hand, since no writer is set up there: the list of proposals, a proposal's blocks and note, accepting one change of two, accepting a new page, "Use the suggestion", and a request refused for lack of a writer.
- **Not tried with a real writer:** a run, a request and a check from start to end, and how well a writer keeps to the marker lines. "Reject", "Dismiss", "Later" and the warning on a proposal written against an older version were not clicked.
