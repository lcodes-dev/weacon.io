# Plan 29: the docs and the code a task is about

**Status: done** (2026-11-24)

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

- [x] Try both models on the docs of a real project with a list of questions and the pages a person would expect, and choose
- [x] `doc-search`: parts, embeddings and ranking, with its tests
- [x] Server: parts kept up with the pages, and the search
- [x] Server and web app: pages offered for a task, and their files
- [x] The agent's `search_docs`, and the pages and files in `task`
- [x] Docs: `docs/architecture.md`
- [x] Checked by hand on ContentWrite, once it has docs: a task is offered the right pages, and an agent started from it opens the right files first

## Outcome

**The trial, on 24 November.** ContentWrite's 15 pages were cut into 71 parts, and 20 questions worded as tasks were asked, each with the pages a person would expect. Counted is how often the best part was of such a page, and how often one of the best three pages was.

| Model | First | In three | Embedding the 71 parts |
| --- | --- | --- | --- |
| `potion-base-8M`, static, about 30 MB | 19 of 20 | 20 of 20 | under a millisecond |
| `potion-retrieval-32M`, static, about 130 MB | 15 of 20 | 19 of 20 | a millisecond |
| `bge-small-en-v1.5`, a transformer | 18 of 20 | 19 of 20 | 31 seconds, its first start included |
| `all-MiniLM-L6-v2`, a transformer | 16 of 20 | 20 of 20 | 2.5 seconds |

The smallest static model did best, so it is the one: a quarter of the size the plan expected, and no native library. Twenty questions on one project is little to go on; the model is one function to change.

Built as planned, with these differences:

- **A page's parts are made when the project is next searched,** not when the page is saved: its row says which version and which model its parts are of. Nothing was added to the many places that save a page, and no work runs in the background.
- **Exact words are found by the crate itself,** in the parts it already has in hand, in place of SQLite's full-text search: a name from the question is looked for in each part's text. No second index to keep.
- **The model's file is in the machine's Hugging Face folder,** where the crate that loads it keeps it and `HF_HOME` points, not in a folder beside the database.
- **The files under a task's pages are listed, not linked:** the code map has no address for a file yet.
- **Tests in the server stand in for the model** with one that makes texts sharing words alike, so they need no download; the crate's own tests use the real model.
- **Not tested:** parts of another model being made again, since the model is a constant; the same statement that finds a changed page finds those.

- **Tests:** 3 in `doc-search`; 59 on the server, one of them new, and the agent's tools' test grown by `search_docs`.
- **Checked by hand on ContentWrite** with the real model: four tasks were each offered three pages, the right one first for three of them and second for "Add a colleague to my workspace"; the first search took 0.4 seconds, model and all 15 pages included, and later ones 5 milliseconds. Claude Code, in a chat for "Customer was charged twice", used `search_docs` and named the billing files to look at first. The panel lists the files of a task's pages.
- **Not checked:** Codex with `search_docs`; a server that has to fetch the model, since the trial had fetched it on this machine already.
