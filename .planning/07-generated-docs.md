# Plan 07: docs generated from the code map

**Status: planned**

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: a project with a code map gets docs written by an AI model from its code: an "Overview" page and a set of "How it's built" pages, one per area of the code. People read them under "Docs".

This plan comes after plan 06 and builds on its map.

## How it works

**Who writes.** The team's owner picks the team's writer in "Team settings". There are four, and to the rest of the plan they are the same thing: a prompt goes in, text comes out.

| Writer | Paid by | How the server reaches it |
| --- | --- | --- |
| Claude, with an API key | The key the team's owner saves | The Anthropic API |
| OpenAI, with an API key | The key the team's owner saves | The OpenAI API |
| Claude Code, with a subscription | The account signed in on the server's computer | Runs `claude` on the server, with the prompt on its input and no tools |
| Codex, with a subscription | The account signed in on the server's computer | Runs `codex` on the server, the same way |

**Using your own subscription** means the last two rows: a subscription can't be used through an API key, only through the vendor's own program, signed in. So it works when the server runs on a computer where you are signed in to Claude Code or Codex, which is the case for a server you run yourself. Because that account belongs to whoever runs the server, only the server's owner can pick those two writers, and only for a team they own. Running the program on each member's own computer comes with the plan for machines.

**Generating.** A member presses "Generate docs" on the project's docs page. The work takes minutes, so it runs in the background and the page shows its progress. It has three stages, the same for every writer:

1. **Outline.** The writer gets the map as an outline (every file with its symbols) and the README, and answers, as JSON, with the pages to write under "How it's built": for each a title, one sentence on what it covers, and the files to read for it. At most 12 pages.
2. **Pages.** For each page the writer gets the outline of the whole repository and the full text of that page's files, and writes the page in Markdown. Each claim about the code names its file and line.
3. **Overview.** The writer gets the README and the finished pages, and writes the one-page overview.

**Why the map matters here.** The outline of a repository is a small fraction of its text, so stage 1 fits in one request, and stage 2 only pays for the files each page needs.

**Keeping the text of files.** Stage 2 needs it, so the map now also stores each content's text beside its symbols. Nothing is downloaded again at generation time, and the docs always match the map's commit.

**Which branch.** Docs describe the repository's default branch, which must be one of the tracked ones.

**Limits.** A page reads at most about 150,000 characters of code; the files that don't fit are named in the page as not read. A repository whose outline doesn't fit in one request is refused with a message.

**Citations.** A page refers to code as `path:line`. The web app turns each into a link to GitHub at the commit the docs describe.

**Going stale.** Docs record the commit they were written from. When the map has moved on, the docs page says so and offers "Generate again", which replaces every generated page.

**When it fails.** A stage that fails stops the run, keeps the previous docs, and shows the writer's message.

## Database

`apps/server/migrations/0005_docs.sql`:

| Table | Columns | Notes |
| --- | --- | --- |
| `docs` | `id`, `project_id`, `section` (`overview` or `built`), `position`, `title`, `markdown`, `commit_sha` | The generated pages |
| `blobs` | adds `text` | A text file's content; empty for the files plan 06 lists only |
| `teams` | adds `writer`, `writer_key`, `writer_model` | Which of the four, its API key if it has one, and a model name when the default isn't wanted |
| `projects` | adds `docs_state`, `docs_message` | `idle`, `running` or `failed`, and what to show beside it |

## API

| Route | Who | Answer |
| --- | --- | --- |
| `PUT`, `DELETE /api/teams/{id}/writer` | the team's owner | Saves or forgets the writer; a key is never read back |
| `POST /api/teams/{id}/projects/{project_id}/docs` | a member | Starts a generation; refused while one runs, or with no map or no writer |
| `GET /api/teams/{id}/projects/{project_id}/docs` | a member | The state, the commit, and the pages |

## Server code

```
apps/server/migrations/0005_docs.sql  new
apps/server/src/docs.rs               new: the routes and the three stages
apps/server/src/writer.rs             new: one function, prompt in and text out, with the four ways to do it
apps/server/src/map.rs                keeps each content's text
apps/server/src/db.rs                 their queries
apps/server/src/lib.rs                the new routes; the state holds the two API addresses and the two program names
```

No new server dependency: the APIs are called over HTTP with `reqwest`, and the programs are started with tokio.

**The model.** With an API key the default is `claude-sonnet-5-5` for Claude, since docs are long and many and it is the cheaper of the strong models; for OpenAI the team's owner types the model's name. The two programs use whatever model their account is set to.

**The background run** is one spawned task per generation. A server restart loses a running one; the project then says "failed" and can be started again.

**Tests**, in `apps/server/tests/api.rs`, with a fake API the tests start themselves and a small script standing in for the programs.

- Only the team's owner saves the writer, a key is never read back, and only the server's owner can pick a program.
- A program gets the prompt on its input and its output becomes the text.
- A generation asks for an outline, then each page with only that page's files, then the overview, and the pages are stored in order.
- Generating again replaces the pages.
- A failing stage keeps the previous docs and reports the message.
- No map, no writer or a run in progress is refused.

## Web app

- **Team settings** gets a "Docs writer" section: the four choices, a key or model field where one applies, "Save", "Remove".
- **Sidebar.** "Docs" opens the docs of the project last opened.
- **`Docs.tsx`**, at `/projects/{id}/docs`, laid out as the reference:

```
  Storefront docs                        Written from commit 3f2a9c1   [Generate again]
  ─────────────────
  Overview                 How it's built › Checkout
  How it's built
    Checkout               The checkout is a three-step form ...
    Orders                 Totals are computed in `cart/total.ts:41`, which ...
    Testing
```

Markdown is rendered with one new web dependency, `react-markdown`.

## Steps

- [ ] Migration; the map keeps file text
- [ ] `writer.rs`: the four writers
- [ ] The team's writer: routes and the "Docs writer" section
- [ ] The three stages in `docs.rs`
- [ ] The background run and its state
- [ ] Server tests, with the fake API and the stand-in program
- [ ] Web app: the docs page
- [ ] Docs: `docs/architecture.md`
- [ ] Checked by hand: generate the docs of a real repository of yours with your subscription and with a key, and read them

## Left out on purpose

| Left out | Arrives with |
| --- | --- |
| "How it works" pages, on what the product does for its users | A later plan: code alone says little about it |
| "Decisions" pages | People write those |
| Editing a page, and docs written by people | The plan for docs |
| Agents proposing changes to docs for review | The plan for agents |
| Updating only the pages whose files changed | When a full run costs too much |
| Diagrams | When you ask |
| Showing what a run cost | When you ask |
| A writer on each member's own computer | The plan for machines |
| Letting the writer explore the code with its own tools | The plan for agents |
| Docs for branches other than the default | When you ask |
