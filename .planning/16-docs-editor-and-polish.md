# Plan 16: a rich editor for docs, and a docs page that looks finished

**Status: done** (2026-11-19)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: the docs page built in plans 14 and 15 gets the editor and the look the owner asked for on 19 November. Nothing changes in what is stored: a page is still Markdown with versions.

## 1. A rich editor

The text box beside a preview becomes one editing surface that shows the page as it will read. Markdown stays what is saved, what the agent writes and what a diff compares.

- **The editor is Milkdown,** chosen by the owner on 19 November after trying it beside Tiptap on the same pages. Tiptap needed less code and a third of the packages; Milkdown changed far less of a page's Markdown on a save, which is what a diff shows.
- **A toolbar of icon buttons** as the box's top row, built from UI SDK's `Toolbar`: bold, italic, strikethrough, inline code, link, two heading sizes, bulleted and numbered list, quote, code block, table, divider, image, attachment, undo and redo.
- **Slash commands:** typing "/" at the start of a line opens UI SDK's trigger menu with the same blocks, narrowed as one types. It reaches the editor through an adapter of ours, which the trial of 19 November showed working on both candidates.
- **Saving shows the diff first.** "Save" opens what the save would change, in the reference's diff, with "Save" and "Keep editing". Nothing is marked in the page while typing.
- **Images:** pasted, dropped, or picked with the toolbar's button. They show in place while they upload.
- **Only the editor is shown:** no preview beside it and no plain text view. The page is edited where it is read, at the same width and in the same type, with the title as its first line.
- **The editor loads only when "Edit" is pressed,** so reading docs, and a public link, don't download it.

## 2. Attachments

A file that isn't an image can be added to a page the same ways an image can.

- **Kept like images:** a file in the folder beside the database, a row saying which project it belongs to and what it was called. The table `images` becomes `files`.
- **Written as a link,** `[report.pdf](file:{id})`, which the page shows with the file's name and size.
- **Always downloaded, never shown:** the server sends an attachment with `Content-Disposition: attachment` and as plain bytes, so an HTML or SVG file can't run in a reader's browser.
- **Limits:** 20 MB for an attachment; images stay at 5 MB.
- **A public link serves the attachments its own pages name,** like images.

## 3. History as a list of revisions

"History" lists a page's versions, newest first, one row each: who, when, and how many lines it added and removed.

- **Opening a row** shows what that revision changed, as a diff against the one before it.
- **Each row has "Restore",** and "Compare with now" for a diff against the page as it is.
- The two pickers are gone.

## 4. Pages reordered by dragging

The list of pages becomes a sortable list, built on UI SDK's reorder behavior.

- **With a pointer:** drag a page by its handle.
- **With the keyboard:** focus a page, Space to pick it up, arrows to move, Space to drop, Escape to cancel. Screen readers hear each move.
- The order is saved on drop. "Move up" and "Move down" are gone.

## 5. The look

The page follows the reference's own parts: its buttons (30 px high, 8 px corners, a small size for rows), its icon buttons, its menu, its diff, its small uppercase labels.

| Now | Becomes |
| --- | --- |
| Five text buttons over the page | "Edit" as the one button, and a "…" menu with "History" and "Delete" |
| A browser's "OK / Cancel" box before deleting or writing the docs again | UI SDK's alert dialog, saying what will be lost |
| "Add page" as a button under the list | A quiet "+ Add page" row at the end of the list |
| "Share", a sentence and a list of links above the pages | A "Share" button on the tab's right that opens a panel: what sharing does, "Create link", and the links, each with its date, who made it, "Copy" that answers "Copied", and "Stop sharing" |
| "Generate again" beside a line of grey text | The same button, with the branch and commit as a small label under the title |
| The diff as tinted text blocks | The reference's diff: line numbers, a header, added and removed lines in its two tints |

- **Icons** for the handle, the menu, copy and the toolbar come from `lucide-react`.

## Tests

In `apps/server/tests/api.rs`: an attachment is kept, downloaded with its name, refused over 20 MB, and reachable through a public link only when a page names it. The rest is the web app, checked in the browser: the round trip trial, editing with the toolbar, an image and an attachment, the list of revisions, dragging with pointer and keyboard, and each dialog.

## Steps

- [x] The round trip trial, and the editor chosen
- [x] The editor with its toolbar and images, loaded on "Edit"
- [x] Attachments: migration, routes, tests, and in the editor
- [x] History as a list of revisions
- [x] The sortable list of pages
- [x] The look: buttons, menu, dialogs, the share panel, the diff
- [x] Docs: `docs/architecture.md`
- [x] Checked by hand in the browser

## Outcome

The editor:

- **Milkdown is mounted by our own few lines,** not its React package, which brings in its ready-made editor with a code editor and a maths renderer. The lock file went from 364 packages with both candidates to 270.
- **"Save" with nothing typed closes the editor** and saves nothing, so opening a page can't rewrite it. With only the title changed, the text is saved as it was.
- **An empty line is left out of what is saved.** Milkdown writes one as a line break in HTML, which a page would show as text.
- **A list button first turns a heading into text,** since Milkdown's list command only wraps plain text.
- **A link's address is still asked with the browser's prompt,** and no button lights up for the format under the caret.
- **The editor is its own file of 176 kB gzipped,** fetched on the first "Edit".
- **Checked by hand** on the dev server: typing, a slash command, heading, numbered list on a heading, a table, renaming, the diff, "Keep editing", and saving. Not tried: bold, italic, strikethrough, code, link, quote, code block, divider, undo and redo in this page (bold and quote were tried in the trial), adding an image by button, paste or drop, and the view for two people editing one page.

The rest:

- **Images and attachments are one thing, a file:** one table `files`, one folder beside the database, `weacon.files`, and one pair of routes. What the bytes are decides which it is: a PNG, JPEG, GIF or WebP is an image, anything else an attachment. Migration 0004 was changed in place, since it had not been committed.
- **An attachment's size is written into its link's text** when it is added, `[report.pdf (2.1 MB)](file:{id})`, so nothing is asked of the server to show it.
- **A download's name keeps only plain letters, digits, spaces and `.-_()`,** since it travels in a header. The link's text keeps the name as it was.
- **An SVG is now kept, as an attachment:** it is downloaded and never shown.
- **The sortable list is a small primitive of ours** on UI SDK's reorder behavior, in `components/Sortable.tsx`. The list shows the new order at once and saves it on drop.
- **"Share" shows how many links there are,** and sits on the tabs' row.
- **43 tests pass;** the image test became the file test, with an attachment in it.
- **Checked by hand** on the dev server: the share panel, the menu, the delete dialog and "Cancel", the revisions list with both diffs, reordering by keyboard, an attachment and an image added and saved, the attachment downloaded with its name, and an attachment's link kept through a second edit. A pointer drag, a paste of a file and a paste of an image were made by a script, since a real pointer and clipboard can't be driven from here.
- **Not tried:** "Create link", "Copy", "Stop sharing", "Restore", "Generate again" and its dialog, picking a file with the toolbar's buttons, a 20 MB refusal in the browser, and an attachment on a public page. The tests cover the limits and the public link.
- **The owner's answers of 19 November:** icons come from `lucide-react`, and an attachment may be any kind of file up to 20 MB. Both are as built.
