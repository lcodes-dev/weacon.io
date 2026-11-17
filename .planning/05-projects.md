# Plan 05: projects inside a team, and GitHub

**Status: done** (2026-11-17)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: a team holds several projects. A project has a name and, optionally, a GitHub repository picked from the ones the team's GitHub App can reach. A project with no repository is just as valid, and can get one later.

The plan has two parts, built in this order: projects, then GitHub. The GitHub part is about twice the size of the projects part.

## Part 1: projects

**A project** is a name inside a team, with an optional repository. Nothing else belongs to a project yet: tasks, docs and chats arrive in their own plans and will hang off it.

**No repository.** The project is only its name. This is the case for work that isn't code, or isn't on GitHub yet.

**An existing repository.** Picked from a list of the repositories the team's GitHub App can reach, so it can't be mistyped and is known to exist. A team with no app can only make projects without a repository, and the form says how to set one up.

**Changing it.** A project's name and repository can be edited, and the repository added or removed at any time: one form with an optional field, not two kinds of project.

**Who can do what.** Any member of the team creates and edits projects. Only the team's owner deletes one.

## Part 2: GitHub

Weacon connects to GitHub as a **GitHub App**, and each team has its own. The team's owner does two things in "Team settings", each once.

**1. Register the team's app.** They press "Set up GitHub", optionally naming the GitHub organization that should own the app. GitHub shows a prefilled page, they confirm, and GitHub sends them back with a one-time code. The server trades the code for the app's id and private key and saves them on the team. This is GitHub's "app manifest" flow: nothing is copied by hand.

**2. Choose repositories.** They press "Choose repositories", which opens the app's install page on GitHub, and pick all or some repositories of that account.

**Why one app per team.** The app is private, and GitHub lets a private app be installed only on the account that owns it. So whatever the team's app can reach belongs to whoever set it up, and the server never has to work out which team an installation belongs to. One app for the whole server would need a setup step by the server's owner, a GitHub sign-in for each person and a check of who may use which installation.

**Using it.** To list repositories the server signs a short-lived token with the app's private key, asks GitHub for the app's installation, trades for an installation token, and calls GitHub with that. Nothing is stored between requests.

**What the app may do.** Read the code of the chosen repositories, and nothing else. The next step, the code map, needs it; asking now saves every team approving again later. Nothing in this plan reads code yet. Opening pull requests and being notified by GitHub come with the plans that need them.

**No webhook.** The app has none, so the server needs no public address for GitHub to call.

**Removing it.** "Remove GitHub" makes the server forget the app. Projects keep their repository's name. The app itself stays on GitHub until deleted there, and the page links to it.

**When GitHub says no.** If the app was deleted or uninstalled on GitHub, listing repositories fails and the page says so and offers the two steps again.

## Choices

- **One private GitHub App per team,** registered through the manifest flow by the team's owner, with an optional organization name.
- **No webhook.** We call GitHub when we need a list, and keep three columns. Learning everything from webhooks would need a public HTTPS address, a webhook secret and several tables.
- **The return check lives in the browser.** The web app keeps a one-time token in the browser tab to recognise the owner coming back from GitHub, with no table for it.
- **The private key is stored as it is,** like the SMTP password, and noted in improvements.

## Database

`apps/server/migrations/0003_projects.sql`:

| Table | Columns | Notes |
| --- | --- | --- |
| `projects` | `id`, `team_id`, `name`, `repo`, `created_at` | `repo` is `owner/name`, or empty for a project with no repository |
| `teams` | adds `github_app_id`, `github_app_slug`, `github_private_key` | All empty until the team's owner registers the app |

## API

| Route | Who | Body | Answer |
| --- | --- | --- | --- |
| `GET /api/teams/{id}/projects` | a member | | The team's projects (`id`, `name`, `repo`), by name |
| `POST /api/teams/{id}/projects` | a member | `{"name", "repo"?}` | The new project |
| `PUT /api/teams/{id}/projects/{project_id}` | a member | `{"name", "repo"?}` | 204. Leaving `repo` out removes the repository |
| `DELETE /api/teams/{id}/projects/{project_id}` | the team's owner | | 204 |
| `POST /api/teams/{id}/github` | the team's owner | `{"code"}` | 204; the code is traded at GitHub and the app saved |
| `DELETE /api/teams/{id}/github` | the team's owner | | 204; the app is forgotten |
| `GET /api/teams/{id}/github/repos` | a member | | The GitHub account, and the repositories the app can reach as `owner/name` |

`GET /api/teams/{id}` also gives the app's name on GitHub, when there is one.

A project's name can't be empty. A `repo` is accepted only if the team's app can reach it.

## Server code

```
apps/server/migrations/0003_projects.sql  new
apps/server/src/projects.rs               new: the four project routes
apps/server/src/github.rs                 new: the calls to GitHub, and the three GitHub routes
apps/server/src/db.rs                     their queries
apps/server/src/teams.rs                  shares its two membership checks
apps/server/src/lib.rs                    the new routes; the state holds GitHub's API address
```

New dependencies (2): `reqwest` to call GitHub, and `jsonwebtoken` to sign the app's tokens.

**Tests**, in `apps/server/tests/api.rs`. GitHub's API address is part of the router's state, so the tests point it at a small fake GitHub they start themselves.

- A member creates a project with no repository, renames it, and the team's owner deletes it; a member can't delete.
- Someone outside the team gets 404 for its projects.
- Only the team's owner registers the app, and the code is traded for its details.
- A team with an app lists its repositories, and a project takes one of them and loses it again.
- A repository the app can't reach is refused, and so is any repository on a team with no app.
- Removing the app leaves the projects and their repository names.

## Web app

- **Sidebar.** A "Projects" section lists the open team's projects, with "New project" under them.
- **`Project.tsx`**, one page for both `/projects/new` and `/projects/{id}`, since a project is only its settings so far:

```
  New project                       or the project's name

  Name         [ Storefront              ]
  Repository   [ No repository        ▾ ]   lists the repositories the app can reach
               From northwind on GitHub.
               or: Set up GitHub in Team settings to pick a repository.

  [Create]                          or [Save]  [Delete project]  owner only
```

- **Team settings** gets a "GitHub" section for the team's owner, and it is where GitHub sends them back:

```
  GitHub
  Organization  [               ]   optional; empty means your own account
  [Set up GitHub]

  after setup:
  App weacon-northwind · northwind · 3 repositories
  [Choose repositories]  [Remove GitHub]
```

## Steps

- [x] Migration, and the project queries in `db.rs`
- [x] `projects.rs`: the four routes, without repositories yet
- [x] Web app: the sidebar's project list and the project page
- [x] `github.rs`: registering the team's app, and the section in team settings
- [x] Listing repositories, and picking one for a project
- [x] Server tests, with the fake GitHub
- [x] Docs: `docs/architecture.md`; the README needs nothing, since GitHub has no server setting
- [x] Checked in the browser: projects without a repository, against the real server
- [x] Checked by hand: register a real GitHub App, install it on a real account, pick a repository

## Outcome

Built as planned, with these differences:

- One route, `GET /api/teams/{id}/github`, gives the app's name, the account and the repositories, so `GET /api/teams/{id}` is unchanged and there is no separate `/github/repos`.
- The page says the app stays on GitHub after "Remove GitHub", but doesn't link to it: the link differs for a person's app and an organization's.
- The browser check covered a project with no repository and the GitHub form's manifest. The check by hand, from `http://localhost:5173`, showed that GitHub accepts a plain localhost address and an app with no webhook.

## Left out on purpose

| Left out | Arrives with |
| --- | --- |
| Reading code, though the app is allowed to | The next plan, the code map |
| Opening pull requests, being notified by GitHub | The plans for agents and tasks |
| Creating a new GitHub repository for a project | When you ask |
| Typing a repository's name by hand, without an app | When you ask |
| Following a repository that is renamed on GitHub | When it happens; the project is edited by hand |
| More than one GitHub account per team | When you ask |
| GitLab, Gitea and other hosts | When you ask |
| A short key per project, such as `SF` for task `SF-21` | The plan for tasks |
| Moving a project to another team | When you ask |
| Encrypting the app's private key, and keeping installation tokens for their hour | See improvements, once built |
