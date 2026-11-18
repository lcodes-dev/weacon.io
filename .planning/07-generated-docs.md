# Plan 07: docs generated from the code map

**Status: done** (2026-11-18)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: a project with a code map gets docs written by an AI model from its code, in three parts: an "Overview" page, "How it works" pages that walk through what a user does and gets, and "How it's built" pages on the project's own code. People read them under "Docs".

This plan comes after plan 06 and builds on its map.

## How it works

**Who writes.** The team's owner picks the team's writer in "Team settings". There are three, and to the rest of the plan they are the same thing: a prompt goes in, text comes out.

| Writer | Paid by | How the server reaches it |
| --- | --- | --- |
| Claude, with an API key | The key the team's owner saves | The Anthropic API |
| Claude Code, with a subscription | The account signed in on the server's computer | Runs `claude` on the server, with the prompt on its input and no tools |
| Codex, with a subscription | The account signed in on the server's computer | Runs `codex` on the server, the same way |

**Using your own subscription** means the last two rows: a subscription can't be used through an API key, only through the vendor's own program, signed in. So it works when the server runs on a computer where you are signed in to Claude Code or Codex, which is the case for a server you run yourself. Because that account belongs to whoever runs the server, only the server's owner can pick those two writers, and only for a team they own. Running the program on each member's own computer comes with the plan for machines.

**How a program is run.** In an empty temporary folder, with the prompt on its input and its answer read from its output. `claude` runs with `--print`, no tools and none of the folder's or the person's own settings; `codex exec` runs with its read-only sandbox. So neither can touch anything, and what a repository's text says can only make the docs wrong, never make the server do something.

**The two kinds of page.**

*How it works* is for the people who use the product. Each page is one flow, told as steps: the user does this, clicks that, and gets this result. It says what happens, in the words the product's own screens use, and names no file, function or framework. It ends with one line listing the files it was written from, so a reader can check it.

*How it's built* is for the people who work on the code. Each page covers one area and explains it the way a colleague would at a whiteboard, not as a list of functions. It has up to four parts:

1. **What this part is for**, in two or three sentences.
2. **How it flows.** The main paths through the code, told in order: what starts it, what happens next and why, where the data ends up. Each step names its file and line. A function is mentioned when the story passes through it, not because it exists.
3. **Handle with care.** The pieces that matter more than they look: a function many things depend on, a check that guards money or access, an order of steps that must not change. For each, why it matters and what would break if it were changed carelessly, or why it is safe to change when it looks frightening.
4. **Possible problems.** Weaknesses or bugs the code itself shows, such as a missing check, a case that isn't handled, or two places that disagree. Each says where it is, why the writer thinks so, and that it is a reading of the code, not a confirmed bug. The part is left out when there is nothing to say; the writer is told not to invent problems to fill it.

**Written for people.** Both kinds of page use plain sentences and short paragraphs, explain a term the first time it appears, start with what the reader most needs, and leave out anything that only restates the code.

**Project code, not framework code.** A repository often holds code the project didn't write: a framework's scaffolding, a vendored package, generated bundles. The docs are about the project's own code. So in the first stage the writer also names the folders that hold someone else's code or generated files, and no page is written about them. A page mentions the framework only where a reader needs it to follow the flow, in a sentence, such as "the framework runs this check before every request". Files a framework generated and the project never changed get the same treatment. The docs page lists the folders that were left out, so a wrong call is visible.

**Generating.** A member presses "Generate docs" on the project's docs page. The work takes minutes, so it runs in the background and the page shows its progress. It has three stages, the same for every writer:

1. **Outline.** The writer gets the map as an outline and the README, and answers, as JSON, with three things: the folders that aren't the project's own code; up to 8 flows for "How it works"; and up to 8 areas for "How it's built". Each page has a title, one sentence on what it covers, and the files to read for it. The outline lists every file, and for a file the map reads its first 40 symbols, which keeps a minified bundle from filling it.
2. **Pages.** For each page the writer gets the outline of the whole repository and the full text of that page's files with their line numbers, and writes the page in Markdown, following the rules of its kind. Pages are written one after the other.
3. **Overview.** The writer gets the README and the finished pages, and writes the one-page overview: what the product is for, how the code is laid out, and the few cautions and possible problems from the pages that matter most, each linking to its page.

The pages are saved together at the end, so the previous docs stay readable during a run and after a failed one. While it runs the page says where it is, such as "Writing page 3 of 9".

**What the writer can know.** Only what the code shows. A flow is read from routes, screens and the code behind them, so it describes what the product does, not why, and a screen built outside the repository isn't in it. Pages say so plainly where the code doesn't settle something, and don't guess.

**Why the map matters here.** The outline of a repository is a small fraction of its text, so stage 1 fits in one request, and stage 2 only pays for the files each page needs.

**Keeping the text of files.** Stage 2 needs it, so the map now also stores each content's text beside its symbols, for text files up to the 500 KB the map already reads. Nothing is downloaded again at generation time, and the docs always match the map's commit. The map's reader number goes up by one, so every existing map is read again and gains its text.

**Which branch.** Docs are written from one of the project's tracked branches. With one tracked branch there is nothing to choose; with several, "Generate docs" asks which. The docs record the branch and the commit.

**Limits.** A page reads at most about 150,000 characters of code; the files that don't fit are named in the page as not read. A repository whose outline is over 400,000 characters is refused with a message. A full run therefore sends the writer at most about 3 million characters, roughly 750,000 tokens, and takes several minutes. Flows are usually far smaller than the limit.

**Citations.** A "How it's built" page refers to code as `path:line`. The web app turns each into a link to GitHub at the commit the docs describe, but only when the map has that file and it is that long; a citation the writer made up stays plain text.

**Going stale.** Docs record the commit they were written from. When the map has moved on, the docs page says so and offers "Generate again", which replaces every generated page.

**When it fails.** A stage that fails stops the run, keeps the previous docs, and shows the writer's message.

## Database

`apps/server/migrations/0005_docs.sql`:

| Table | Columns | Notes |
| --- | --- | --- |
| `docs` | `id`, `project_id`, `section` (`overview`, `works` or `built`), `position`, `title`, `markdown` | The generated pages |
| `blobs` | adds `text` | A text file's content; empty for the files plan 06 lists only |
| `teams` | adds `writer`, `writer_key`, `writer_model` | Which of the three, its API key if it has one, and a model name when the default isn't wanted |
| `projects` | adds `docs_state`, `docs_message`, `docs_branch`, `docs_commit` | `idle`, `running` or `failed`, what to show beside it, and what the docs were written from |
| `projects` | adds `docs_left_out` | The folders the writer judged not to be the project's own code |

## API

| Route | Who | Answer |
| --- | --- | --- |
| `PUT`, `DELETE /api/teams/{id}/writer` | the team's owner | Saves or forgets the writer; a key is never read back |
| `POST /api/teams/{id}/projects/{project_id}/docs` | a member | Starts a generation from the `branch` given; refused while one runs, or with no map of that branch or no writer |
| `GET /api/teams/{id}/projects/{project_id}/docs` | a member | The state, the branch and commit, the folders left out, and the pages |

## Server code

```
apps/server/migrations/0005_docs.sql  new
apps/server/src/docs.rs               new: the routes and the three stages
apps/server/src/writer.rs             new: one function, prompt in and text out, with the three ways to do it
apps/server/src/map.rs                keeps each content's text
apps/server/src/db.rs                 their queries
apps/server/src/lib.rs                the new routes; the state holds the API's address and the two program names
```

No new server dependency: the API is called over HTTP with `reqwest`, and the programs are started with tokio.

**The model** is a cheap one by default, since a run reads a lot and docs are regenerated often. The team's owner can type another.

| Writer | Default model |
| --- | --- |
| Claude, with an API key | `claude-haiku-5-5` |
| Claude Code | `claude-haiku-5-5`, passed as `--model` |
| Codex | A "luna" model, passed as `-m`. This computer's `codex` knows `gpt-5.6-luna` and `gpt-6-luna`; which one your account can use is settled when building |

On this computer `claude` 2.1.319 and `codex` 0.159.2 are installed and have the options this plan needs; neither has been run this way yet.

A cheap model is the weak point for "Handle with care" and "Possible problems", which ask for judgment, not summary. The rules above keep it honest: every such claim must point at a line, say why, and be marked as a reading. If the first real run reads thin or wrong there, the model's name is the one setting to change.

**The background run** is one spawned task per generation. A server restart loses a running one, so at startup every project still marked `running` is marked `failed`, and can be started again.

**Tests**, in `apps/server/tests/api.rs`, with a fake API the tests start themselves and a small script standing in for the programs.

- Only the team's owner saves the writer, a key is never read back, and only the server's owner can pick a program.
- A program gets the prompt on its input and its output becomes the text.
- A generation asks for an outline, then each page with only that page's files and the rules of its kind, with the default cheap model unless the team set another, then the overview, and the pages are stored in order under their section.
- No page is asked for about a folder the outline left out, and the left-out folders are reported.
- Generating again replaces the pages.
- A failing stage keeps the previous docs and reports the message.
- No map, no writer or a run in progress is refused.

## Web app

- **Team settings** gets a "Docs writer" section: the three choices, a key or model field where one applies, "Save", "Remove".
- **The project's page** gets a "Docs" link beside its name.
- **`Docs.tsx`**, at `/projects/{id}/docs`, laid out as the reference:

```
  Storefront docs                        Written from main at 3f2a9c1   [Generate again]
  ─────────────────
  Overview                 How it works › Start a return
  How it works
    Buy                    1. The customer opens an order and chooses "Return items".
    Start a return         2. They tick the items and pick a reason.
  How it's built           3. They get a prepaid label by email within a few minutes.
    Checkout
    Orders                 Written from: routes/web.php, resources/js/Pages/Returns.vue
  Left out: spark-paddle, public/js
```

Markdown is rendered with one new web dependency, `react-markdown`.

## Steps

- [x] Migration; the map keeps file text
- [x] `writer.rs`: the three writers
- [x] The team's writer: routes and the "Docs writer" section
- [x] The three stages in `docs.rs`, with the rules for each kind of page
- [x] The background run and its state
- [x] Server tests, with the fake API and the stand-in program
- [x] Web app: the docs page
- [x] Docs: `docs/architecture.md`
- [x] Checked by hand: generate the docs of a real repository of yours with your subscription and with a key, and read both kinds of page, checking a few "Possible problems" against the code

## Outcome

Built as planned, with these differences:

- A page's prompt carries the list of the repository's files, not the whole outline with symbols. Sending the outline with every page would have multiplied the size of a run.
- The overview names the page a caution comes from in bold; it doesn't link to it.
- The server, not the writer, appends "Written from" to a "How it works" page, so the list is always true.
- The team's writer is read from `GET /api/teams/{id}`.
- Codex's default is `gpt-5.6-luna`, which answered a test prompt on this computer.
- A real run on `lorenzocattaneo/contentwrite` with Codex and `gpt-5.6-luna` wrote 15 pages in 23 minutes: the overview, 6 flows and 8 technical pages. It left out `spark-paddle`, `storage`, `bootstrap/cache` and two vendor folders. Four "Possible problems" from one page were checked against the code and all four were real, at the lines cited. The Anthropic API key writer has not been run for real.
- A citation is shown as the file's name and line, with the whole path in its tooltip, since a paragraph can carry six of them.
- `claude` on this computer isn't signed in for use outside the desktop app, so the Claude Code writer has run only against the tests' stand-in program. Its options were accepted up to the sign-in check.

## Left out on purpose

| Left out | Arrives with |
| --- | --- |
| An OpenAI API key as a writer | When you ask |
| Telling the writer which folders are or aren't the project's code | When its own call proves wrong |
| "Decisions" pages | People write those |
| Editing a page, and docs written by people | The plan for docs |
| Agents proposing changes to docs for review | The plan for agents |
| Updating only the pages whose files changed | When a full run costs too much |
| Diagrams | When you ask |
| Showing what a run cost | When you ask |
| A writer on each member's own computer | The plan for machines |
| Letting the writer explore the code with its own tools | The plan for agents |
| Docs for branches other than the default | When you ask |
