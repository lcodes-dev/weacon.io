# Plan 14: docs in two tabs, and a public link to the user docs

**Status: done** (2026-11-19)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: a project's docs page shows the user docs and the technical docs as two tabs, and a member can share the user docs, as they are at that moment, through a link anyone can open.

## 1. Two tabs

Today one list holds all pages under two headings. It becomes two tabs, built with UI SDK's `Tabs`:

| Tab | Pages |
| --- | --- |
| User docs | The "How it works" pages: what a user does and gets. Their prompt already forbids naming a file, function or table |
| Technical docs | The overview, then the "How it's built" pages. The overview describes how the code is laid out and lists cautions, so it belongs here |

- **The open tab and page are in the address,** as `?tab=technical&page={id}`, so a link to a page can be passed around inside the team.
- **A tab with no pages is not shown.**

## 2. A public link

"Share" on the user docs tab makes a link, `{server}/shared/{token}`. Anyone with it reads the user docs, without an account, on a page that shows only them.

- **A link is a copy.** Sharing copies the user pages as they are at that moment. Writing the docs again, or editing them later, changes nothing behind a link; a newer state needs a new link. A project can have several.
- **What a link shows:** the project's name, the day it was shared, and the user pages. Not the repository, the branch, the commit, the team, the technical docs or any other project.
- **The token** is 128 random bits, as a session's is. It is the only thing that guards the page, so whoever has the link can pass it on.
- **Ending a link:** the docs page lists the project's links with their day and who made them, each with "Copy" and "Stop sharing". Deleting the project ends its links. Changing the project's repository does not: the copy stays what it was.
- **The public page asks search engines not to list it.**
- **Images stay links,** as on the docs page, so opening a shared page fetches nothing from another host.

### Data

One migration:

```
shares        token (key), project_id → projects (cascade), title, created_by → users, created_at
              index on project_id
share_pages   token → shares (cascade), position, title, markdown; key (token, position)
```

### API

| Route | Who | What |
| --- | --- | --- |
| `GET`, `POST /api/teams/{id}/projects/{project_id}/shares` | a member | The project's links, or makes one from the user pages; refused when there are none |
| `DELETE /api/teams/{id}/projects/{project_id}/shares/{token}` | a member | Ends a link |
| `GET /api/shares/{token}` | anyone | `{"title", "created_at", "pages"}`, or 404 |

### Web app

- `routes/Shared.tsx` at `/shared/:token`, beside the invite route and outside the signed-in shell.
- `components/DocPages.tsx`: the list of pages and the open page, used by both the docs page and the public page. On the public page a `path:line` stays plain text.

## Tests

In `apps/server/tests/api.rs`:

- A shared link shows the user pages and none of the technical ones, without a session.
- Writing the docs again leaves what a link shows unchanged.
- An ended link, and a link of a deleted project, answer 404.
- Someone outside the team can't make, list or end a project's links.
- A project without user pages can't be shared.

The tabs and the public page are checked in the browser.

## Steps

- [x] The migration and queries, with `.sqlx/` written again
- [x] The routes, with their tests
- [x] The docs page in two tabs, with the open tab and page in the address
- [x] "Share", the list of links, and the public page
- [x] Docs: `docs/architecture.md`
- [x] Checked by hand in the browser, signed in and signed out

## Outcome

Built as planned, with these differences:

- **A copy leaves out the lines a run adds under a page,** "Written from:" and "Not read, for lack of room:", since they list the repository's files. The plan had missed that user pages carry them.
- **The copy is made by the server from the pages it read,** not by one statement in the database, for that reason.
- **40 tests pass,** two of them new: one for sharing, reading without a session, a stranger's refusals and ending a link, and one for a deleted project.
- **Checked by hand** on the dev server, with four sample pages put into its database by hand, since no writer is set up there: both tabs, a page opened from its address, "Share", and the public page, which was also fetched without a session. "Copy link" and "Stop sharing" were not clicked; the test covers ending a link.
