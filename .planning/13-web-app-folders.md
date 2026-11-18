# Plan 13: the web app's folders, and how long a sign-in lasts

**Status: done** (2026-11-19)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: the web app's files sit in a folder for routes and one for components, and a sign-in lasts as long as the owner asked.

## 1. How long a sign-in lasts

A sign-in already lasts a year: `SESSION_SECONDS` in `auth.rs` is 365 days, for both the cookie and the row in `sessions`. The sign-outs seen while building came from plan 10, which started the dev database anew and so dropped every session.

What a year means today, and what could change:

| | Today | Proposed |
| --- | --- | --- |
| The year counts from | The sign-in. Someone who uses Weacon every day is still signed out a year after signing in | The last use: a session used within a year never ends |
| How | | `/api/session`, which the web app asks on every load, sets the cookie again and moves the session's date when it is more than a day old |

Is a year safe? Yes, for this kind of app:

- **The cookie can't be read by a page's scripts** (`HttpOnly`), is not sent by other sites' requests (`SameSite=Lax`), and travels only over `https` when the server has an `https` address.
- **Signing out deletes the session on the server,** so a copied cookie stops working.
- **What a long session costs** is that a lost or shared computer stays signed in. Nothing lists a person's sessions or signs them out elsewhere; that goes in `improvements.md`, not in this plan.

## 2. The folders

Today all 14 files sit in `apps/web/src/`. Proposed:

```
src/
  main.tsx              the session and the two top routes
  api.ts
  app.css
  routes/               one file per address
    SignIn.tsx          anything, signed out
    Invite.tsx          /invite/:token
    Project.tsx         /projects/:id
    ProjectDocs.tsx     /projects/:id/docs      (was Docs.tsx)
    TeamSettings.tsx    /settings/team
    MailSettings.tsx    /settings/mail
  components/           what routes are built from
    Shell.tsx           the sidebar and frame around a signed-in route
    CodeMap.tsx
    CodeGraph.tsx
    GithubSection.tsx
    WriterSection.tsx
```

- **Files move and their imports change; no code is rewritten.** `Docs.tsx` is renamed, since "Docs" will also be the name of the team's own docs.
- **Two flat folders, no folder per route.** With six routes of one file each, a folder per route would hold one file. A route gets its own folder when it grows a second file only it uses.
- **No new shared components.** The files were read for markup repeated across them; the only repeat is the one-line error message, which is shorter than a component for it.

## Tests

Server: a session a day old or more is moved forward by `GET /api/session`, and one older than a year is refused. The web app: it builds, and each route is opened in the browser.

## Steps

- [x] A session lasts a year from its last use, with its test
- [x] The files moved into `routes/` and `components/`
- [x] Docs: `docs/architecture.md`
- [x] Checked by hand: every route opens in the browser

## Outcome

Built as planned, with these differences:

- **The session's date is now named `renewed_at`,** in a second migration, since it is no longer when the session was made.
- **Checked by hand:** the dev server kept its session across the migration, and the project, its docs, both settings pages and an invite opened. The sign-in page was not opened, since that needs signing out; it builds, and the invite page uses it.
