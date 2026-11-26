# Plan 40: closer to the reference, and phones

**Status: done** (2026-11-26)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: the web app looks like `.planning/reference_ui.html`, logo included, and works well on a phone. Today the sidebar is plain text with no logo, Tasks and Docs go nowhere, and on a phone the 272px sidebar stays open and squeezes every page into the last 120px.

What changes is only how things look and where they sit. No new feature from the reference comes with it: search, direct messages, the theme switch and a task's checklist stay out until a plan of their own asks for them.

## The logo

- **The Interlock mark,** moved from `apps/server/assets/logo.svg` to `apps/web/public/logo.svg`, at 22px beside "weacon" in 16px bold, as the reference draws it.
- **The same file is the tab's icon,** served at `/logo.svg`. Mail's `logo.png` is still made from it.

## The sidebar

As the reference draws it:

- **Rows** 32px high, 7px rounded, with a 7px dot or a small icon on the left and a grey second line where there is one. The row of the page that is open is raised: the surface color, a hairline border, bold.
- **Section names** in 10.5px mono capitals, with more space above them.
- **Tasks and Docs** get their red and violet square icons and open the tasks and docs of the project that is open, or else the first. They are hidden while the team has no project. The project page's own Docs and Tasks links go, since the sidebar now has them.
- **Groups** show `#`, their project or how many people are in them, and a red dot while unread. The server says only whether a group is unread, not how much, so there is no count.
- **At the bottom,** two cards: the team, with its initials in a green square, its member count and, while you are in several, the team switch; and you, with your initials, your email, and icon buttons for team settings and sign out. The mail warning stays above them for the owner.

## The pages

- **One header bar,** 52px with a hairline under it, holding the page's title and its actions, on the chat, tasks and group pages. Settings and the docs keep their centered page.
- **List and Board** become a segmented control.
- **The tasks list** becomes the reference's table: key, title, status chip and owner, in a bordered card. The reference's agent column is left out: a task's chats are in its panel.
- **The composer** of a chat and a group is the reference's rounded box, with Send in a row under the text.

## Phones

Below 720px wide:

- **The sidebar is a drawer.** A top bar shows a menu button and the logo, above the page's own header; the button slides the sidebar in over the page, and choosing anything in it, tapping outside it or Escape closes it. With no page open, the sidebar is the page.
- **Side panels and columns stack,** full width: the docs' list of pages, the code map's details, the board's columns. A task's panel takes the place of the list, which its close button brings back.
- **Everything you tap is at least 44px tall.** Text inputs are 16px so that iOS doesn't zoom into them.
- **The composer stays at the bottom** of the screen, above the home bar (`env(safe-area-inset-bottom)`).
- **Nothing scrolls sideways** except a diff or code, inside its own box.

## Checks

- A Playwright check, `apps/web/e2e/phone.test.ts`, run by `scripts/check.sh` against the server on a fresh database: at 390px wide it opens each page and checks that nothing scrolls sideways, that every button, link and field is at least 44px tall (an underlined link in text is exempt, as WCAG 2.5.8 allows), and that the drawer opens and closes.
- Before and after screenshots, desktop and phone, in the pull request.

## Steps

- [x] The logo in the sidebar and as the tab's icon
- [x] The sidebar's rows, sections, Tasks and Docs, groups and the two cards at the bottom
- [x] One header bar, the segmented control, the tasks table and the composer
- [x] Phones: the drawer, side panels under the page, 44px targets, the composer at the bottom
- [x] The phone check in `scripts/check.sh`
- [x] `docs/architecture.md` says how the web app handles phones; the sidebar's item leaves `improvements.md`
