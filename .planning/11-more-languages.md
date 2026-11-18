# Plan 11: more languages in the code map

**Status: done** (2026-11-18)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: the map reads the common languages and a handful of less common ones, 21 in place of 5.

## Why not every language

The map already lists every file of any language, with its line count, and keeps its text for the docs writer. What a language needs support for is the rest: which names a file defines, and which it mentions, from which the "uses" lines are drawn.

That takes two things per language, both built into the server:

| Needed | What it is |
| --- | --- |
| A grammar | A parser for the language, published as a crate; most are tens of thousands of lines of generated C |
| A tags query | A short file saying which parts of the parsed code define a name and which mention one |

So a language nobody has written a grammar for can't be read, and one whose grammar ships no tags query needs us to write that query. "Any language" would mean guessing at definitions with patterns over plain text, which gives wrong lines on the map; this plan doesn't do that.

## What was checked

60 grammar crates on crates.io were downloaded and read. The owner then chose the list on 18 November:

| Group | Languages | Work for each |
| --- | --- | --- |
| Grammar and tags query, ready | Go, Java, C, C++, C#, Ruby, Swift, Dart, Elixir, Lua | One dependency and one entry in `map.rs` |
| Grammar, and a tags query we keep ourselves | Kotlin, Scala, Bash, Zig | The same, plus a query file in `apps/server/assets/tags/`. Scala's comes from its grammar's repository, which has one but doesn't publish it; the others we write |
| No grammar of their own | Vue, Svelte | Only the scripts are parsed, as TypeScript. The file is a component named after itself, and a capitalised tag mentions a component |
| Already read | React, as `.jsx` and `.tsx` | A component's tag now counts as a use of it, which it didn't |

With today's JavaScript, TypeScript, Rust, Python and PHP that makes 21.

## How it works

Nothing changes in how the map reads: `LANGUAGES` in `map.rs` gets an entry per language, with its file extensions.

- **`.h` is read as C.** A C++ header named `.h` loses its classes; `.hpp`, `.hh` and `.hxx` are read as C++.
- **The reader's number goes up,** so every map is read again once.
- **How well a language is read depends on its tags query,** which is someone else's work for 11 of them.

## Tests

Two tests in `apps/server/tests/api.rs`: a repository with one small file per added language, where the map shows each file's language and a name it defines; and one with React, Vue and Svelte components, where each use of a tag is a line on the map.

## Steps

- [x] The ten ready languages
- [x] Kotlin, Scala, Bash and Zig, with their query files
- [x] Vue and Svelte scripts, and component tags in all three
- [x] The tests
- [x] Docs: `docs/architecture.md`
- [x] Checked by hand: the dev server maps a repository again with the new reader

## Outcome

- **The server's binary went from 23 MB to 55 MB,** and a clean release build from 45 to 50 seconds.
- **The first map after a start takes about a second longer,** while the 21 queries are prepared; the tests went from 0.6 to 1.9 seconds for that reason.
- **C#'s own tags query has a mistake:** one capture is named `@module`, which the reader refuses. It is corrected as the query is loaded.
- **Checked by hand:** the dev server read `lorenzocattaneo/contentwrite` again. Its 68 Vue files, listed without a language before, are now components, and the map went from 133 lines to 261.
- **A lower-case tag is never a component:** `<div>` in JSX mentions nothing.
- **Vue's `<my-card>` way of writing a tag is not matched** to `MyCard.vue`; only `<MyCard>` is.
- **Every command in a shell script is a mention,** `ls` and `echo` included. They link nothing unless a file defines a function of that name.

## Left out on purpose

| Left out | Why |
| --- | --- |
| R, OCaml, Elm, Gleam, Solidity, Protocol Buffers | Ready, with a grammar and a tags query; not on the owner's list. One entry each, when you ask |
| Haskell, Julia, Objective-C, Perl, PowerShell | Each has a grammar and no tags query. A query of ours each, when you ask |
| Nix, D, Fortran, MATLAB | A tags query exists in the grammar's repository and isn't published. Copied like Scala's, when you ask |
| SQL, Clojure, COBOL, Dockerfile, Vim script | Their crates are built for a tree-sitter too old to use beside ours |
| HTML, CSS, JSON, YAML, TOML, Markdown | They define nothing a file could "use"; they stay listed with their line count |
| Guessing at definitions in any other language | Wrong lines on the map are worse than none |
