# Plan 29: the docs and the code a task is about

**Status: planned**

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: Weacon finds the pages of a project's docs that a piece of text is about, by meaning and not only by its words, and from those pages the code. A task is then offered the docs it touches, and an agent can ask for what the docs say on a subject. The owner asked for it on 24 November: docs get embeddings, and with the code map a task's references can be exact. It needs plan 27, and plan 28 for the agent's part.

## Why the references can be exact

A page of the docs already knows the files it was written from and at which commit, and the code map knows what each file defines and which files it uses. So nothing about code has to be guessed from text:

1. **Text to pages:** by search, which this plan builds.
2. **Pages to files:** by what each page records, which exists.
3. **Files to their neighbours:** by the code map, which exists.

Only the first is a guess, and a person confirms it: a page is offered for a task, and it is the task's only once picked.

## The search

- **A page is cut at its headings,** and each part is kept with its page, its heading and its text.
- **Each part gets an embedding:** a list of numbers that is near another's when the two say something alike. They are worked out on the server when a page's text changes, and kept in the database beside the part.
- **A search embeds the question** and ranks a project's parts by nearness, in the server's own code: a project has some hundreds of parts, so every one is compared and no index of vectors is needed.
- **Exact words count too.** A part that holds a name from the question as written, such as a function's or a file's, is ranked above one that is only alike in meaning. SQLite's own full-text search finds those.

## Which model, to be settled by trying

Two ways to embed on the server were looked up on 24 November. Neither sends a team's docs anywhere.

| | A small static model | A small transformer |
| --- | --- | --- |
| Crate | `model2vec-rs` | `fastembed` |
| What it adds to the server | One crate and a model file of about 130 MB | The same, and ONNX Runtime, a native library fetched when the server is built |
| How well it finds, by its makers' own figures | About four fifths of the transformer it is compared with | The reference |
| Speed | A page in well under a millisecond | Slower by orders of magnitude, still enough for docs |
| Languages | English; a larger one for many languages exists | English, with others to pick |

**Proposed: the static model, with the exact-word search beside it,** since it adds no native library to a server that is one binary, and a team's docs are few. The first step tries both on real docs before anything is built on one, and the transformer is taken if the static one misses what a person would expect to find.

The model's file is downloaded once, when the server first needs it, into a folder beside the database. A server with no way out to the internet can be given the file by hand. The owner agreed to this on 24 November, for now, and to English.

## Easy to swap

The owner asked that another way to embed, such as a vendor's API, can take this one's place without the rest changing. So:

- **Embedding is one function,** in a file of its own: texts in, a list of numbers for each out. Cutting pages, keeping parts and ranking them know nothing of where the numbers come from, nor how many there are.
- **Every part records which model made its numbers.** When the server's model is another, it embeds every part again, in the background, and searches only parts of the current model meanwhile. The code map does the same when its reader changes.
- **A second way is then that one function written again.** No setting to choose between two is built until there are two. An API would be called from the server with a key of the team's, and would send the docs' text to that vendor, which this plan's model does not.

## What is built on it

- **On a task:** under "Docs it touches", up to three pages are offered from the task's title and text, each added with one press. Under them, the files those pages were written from, each a link into the code map.
- **For an agent:** a tool, `search_docs`, that takes a sentence and answers with the best parts: page, heading, text, and the files of the page. An agent started from a task is given the task's pages and their files by `task`, so it starts in the right place of the code.

## Where the code goes

| Where | What |
| --- | --- |
| `crates/doc-search/` | Cutting a page into parts, embedding, and ranking, with the embedding in a file of its own. It has its own rules and tests, and the server only asks it for parts and for an order |
| `apps/server/migrations/` | A table of parts, with each one's embedding, and the full-text index over them |
| `apps/server/src/db.rs`, `pages.rs`, `docs.rs` | A page's parts are written again when its text changes, by a person or by a run |
| `apps/server/src/tasks.rs`, `tools.rs` | The pages offered for a task; the agent's tool |
| `apps/web/src/routes/ProjectTasks.tsx` | The offered pages and the files |
| `docs/architecture.md` | How docs are searched, and where the model comes from |

## Not in this plan

- Embeddings of the code itself. The code is reached through the docs and the map.
- A search box for people over the docs.
- Docs that change by themselves when a task is done.

## Tests

- In `crates/doc-search/tests/`: a page is cut at its headings; of a handful of made-up pages, a question finds the one about it, also when it shares no word with it; a name written exactly ranks its part first.
- In `apps/server/tests/api.rs`: parts made by another model are embedded again; a changed page is found by its new text and no longer by its old; a task is offered a page about it; a page of another project is never offered.
- The model's file is needed for these, so they are skipped with a clear line where it is missing.

## Steps

- [ ] Try both models on the docs of a real project with a list of questions and the pages a person would expect, and choose
- [ ] `doc-search`: parts, embeddings and ranking, with its tests
- [ ] Server: parts kept up with the pages, and the search
- [ ] Server and web app: pages offered for a task, and their files
- [ ] The agent's `search_docs`, and the pages and files in `task`
- [ ] Docs: `docs/architecture.md`
- [ ] Checked by hand on ContentWrite, once it has docs: a task is offered the right pages, and an agent started from it opens the right files first
