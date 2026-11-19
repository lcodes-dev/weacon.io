# Plan 17: who uses what, by imports, in every language

**Status: done** (2026-11-19)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: a line in the code map means what it says. Today a file "uses" another when it mentions a name that only that other file defines, anywhere in the repository. On a Laravel project that drew most lines to a minified bundle and missed the real ones. The map gets one rule that works the same way in all 21 languages it reads, in a crate of its own.

## What went wrong, measured on a real project

422 files, 248 of them PHP, 68 Vue:

- **The PHP reader kept almost no class names.** Of a file with `use Actions\AI\Objects\AIGenerationRequest;` and `new AIGenerationRequest()` it kept only the names of the methods it calls.
- **A method's name was matched against every file in every language.** A minified JavaScript bundle that happens to define `create` and `post` took 96 of the map's 260 lines.
- **A name two files define was dropped,** which lost 37 of 132 class names.
- **Imports were not read,** in any language, though they say exactly who uses what.

## The rule

A file says three things about itself, and every language's file says the same three:

| | What | PHP | TypeScript | Go |
| --- | --- | --- | --- | --- |
| **Its scope** | The name its definitions live under, when the language has one | `namespace App\Models` | none | `package store` |
| **Its imports** | What it says it needs, as written | `App\Models\User` | `./price`, `@/lib/api` | `shop/internal/store` |
| **Its mentions** | The names it uses, and whether each is written after a dot or an arrow | `User`, `->save` | `format`, `.toFixed` | `store.Open` |

**An import is found** by trying, in this order, and stopping at the first that gives a file:

1. **As a full name:** a scope plus a definition (`App\Models\User`), or a scope alone, which stands for every file in it (`shop/internal/store`).
2. **As a path beside the file that imports,** with the language's endings tried: `./price` is `price.ts`, `price/index.ts`.
3. **As the end of a path,** dropping the import's leading parts one at a time: `@/lib/api` is the file whose path ends in `lib/api.ts`. An import of one part alone is never matched this way.

Where two files fit, the one that shares the longest folder path with the importing file wins. An import that finds nothing is the language's own library or a package, and draws no line.

**A file uses another** when:

1. it imports that file; or
2. it mentions a name that a file in a scope it imports defines, or a file in its own scope, which for a language without scopes is its own folder; or
3. it mentions a name, not written after a dot or an arrow, that exactly one file in the same language defines.

The third is the only guess. It is what Swift needs, where a file imports nothing from its own module, and Ruby on Rails, where classes are found by name.

Nothing here reads `composer.json`, `tsconfig.json`, `go.mod` or any other settings file, and no language has code of its own: step 3 of finding an import does what those files would say. Tried on the project above with rough stand-ins for the readers, the rule drew 453 lines against 260, none to the bundle, and found all 8 files that use `AIGenerationRequest`.

## What each language gives

One tags query per language, ours, in the crate. Each marks definitions and mentions as today, and adds the scope and the imports. Beside it, one line of data: the endings to try for a path.

| Language | Scope | Imports |
| --- | --- | --- |
| JavaScript, TypeScript, Vue, Svelte | none | `import … from`, `import()`, `require()`, `export … from` |
| PHP | `namespace` | `use`, also grouped and aliased; `require` and `include` of a path |
| Python | none | `import a.b`, `from a.b import c`, and the same with leading dots |
| Rust | none | `use` and `mod`, with `crate`, `super` and `self` |
| Go | `package`, with its folder | `import` |
| Java, Kotlin, Scala | `package` | `import`, also with `*` |
| C# | `namespace` | `using` |
| C, C++ | none | `#include "…"` |
| Ruby | none | `require_relative`, `require` |
| Swift | none | none inside a module, so the guess carries it |
| Dart | none | `import`, `export`, `part` |
| Elixir | none; a module's own name is full | `alias`, `import`, `use`, `require` |
| Lua | none | `require` |
| Bash | none | `source` and `.` |
| Zig | none | `@import` |

- **The queries shipped with the grammars are no longer used.** They differ too much in what they mark; PHP's misses `new Foo()`.
- **A language is done when its fixture passes** (see Tests), so none ships half read.

## Files that are not the project's code

- **A minified file is listed but not read:** one whose longest line passes 5,000 bytes.
- **A file in a folder named `vendor` or `node_modules` is listed but not read,** as the owner decided on 19 November.

## The crate

`crates/code-map/`, the first in `crates/`. It holds the 21 languages, their queries and the rule, and knows nothing of GitHub, the database or HTTP. It has two functions:

- **`read(path, bytes)`** gives a file's symbols, scope, imports and mentions.
- **`link(files)`** takes every file of a branch with what was read from it, and gives the pairs of a file and a file it uses.

`map.rs` in the server keeps the queue, the worker, GitHub and the limits, and calls those two.

## Data

One migration:

```
blobs          + scope TEXT
blob_imports   project_id, blob → blobs (cascade), import; key (project_id, blob, import)
blob_mentions  + member INTEGER, in the key: 1 when written after a dot or an arrow
```

- **Linking is done in Rust when the map is asked for,** in place of the SQL statement that does it today, since finding an import depends on where the file sits and one content can sit in two places.
- **`READER` goes up,** so every map is read again at startup.

## Tests

- **In the crate:** one small made-up repository per language, as files in `crates/code-map/tests/`, each with the lines it must draw and one it must not. This is where the 21 languages are proven.
- **In `apps/server/tests/api.rs`:** the map tests that exist, with what they expect brought in line, and one for a minified file.
- **By hand:** the real project above, and one each in TypeScript and Rust, looked at in the browser.

## Left out on purpose

| Left out | Why |
| --- | --- |
| Which method of which class a call reaches | Needs the types of variables; both tools we studied do it with thousands of lines per language. A line between files doesn't need it |
| Reading settings files such as `composer.json` or `tsconfig.json` | Finding an import by the end of its path gives the same answer without them |
| A real indexer per language, run on the project | Needs the project's own tools and packages installed on the server |
| A ready-made crate | None fits: one is archived and has no PHP, one is unpublished under an unclear licence, the rest need the project built |

## Steps

- [x] The crate, with `read` and `link`, moved out of `map.rs` with today's behavior and tests green
- [x] The rule in `link`, and scope, imports and member mentions in `read`
- [x] Our queries and fixtures: JavaScript, TypeScript, Vue, Svelte, PHP
- [x] Our queries and fixtures: Python, Rust, Go, Java, Kotlin, Scala, C#
- [x] Our queries and fixtures: C, C++, Ruby, Swift, Dart, Elixir, Lua, Bash, Zig
- [x] Minified files left unread
- [x] The migration, `map.rs` calling the crate, `.sqlx/` written again, `READER` raised
- [x] Docs: `docs/architecture.md`
- [x] Checked by hand on three real projects

## Outcome

Built as planned, with these differences:

- **The queries shipped with the grammars stay,** as each language's first half. Ours come after and add the scope, the imports and the mentions the shipped ones miss. Rewriting all 21 from nothing would have been more to get wrong for the same result; PHP's `new Foo()` is fixed by a line of ours.
- **Whether a mention is a member is read from the tree,** by the dot, arrow or double colon before it, so no query marks it.
- **The guess is narrower than planned:** only a name that starts with a capital, and not one the file imports from outside the repository. Without the first, a call to `fetch` found the one file that defines a `fetch`; without the second, `Request` from a package found the project's own `Request`.
- **Two more ways to find an import:** as a path from the repository's root, which `import shop` in Python needs, and with its last part dropped, which `crate::db::Doc` and a static import in Java need.
- **Two tie-breaks:** of several files that define a full name, the one whose path follows the name further wins, so `App\Models\User` is `app/Models/User.php` before a second copy in `Models/User.php`; then the nearest by folder.
- **Of several files in a scope that define a mentioned name, the nearest is used,** not all of them.
- **TypeScript's type aliases and enums are now symbols.** The shipped query left them out, and a type imported by name was guessed to be a function of the same name elsewhere.
- **Scala's imports are read as whole lines** and taken apart in the reader, since its grammar gives an import's path in pieces. It is the one place the reader looks at an import's text.
- **Two small tools came with the crate,** as examples: one maps a folder on this computer, one prints how a file is parsed and read.
- **The owner's answers of 19 November:** a guessed line looks like any other for now, until it is known how many lines are guesses; and a crate has its own tests, which `AGENTS.md` now says. And files in a folder named `vendor` or `node_modules` are listed and not read, like minified ones; a package copied into the repository under another name is still read.
- **Tests:** 16 in the crate, one per language or family of languages, each with files that must not be linked; 46 in the server, where the map's main test gained a minified bundle.
- **Checked on real projects:** the Laravel project through the server, in the browser: 484 lines against 260, none to the minified bundle, all 8 users of `AIGenerationRequest.php` found, and `WriteText.php` using the three files it imports. With the crate's tool on folders: this repository (Rust and TypeScript), a Swift project, a Python project and an Elixir project. The lines of this repository were read through one by one; the others were looked over.
- **Speed:** the map of the Laravel project, 422 files, is answered in about 50 ms on the dev server, linking included.
- **Not tried on a real project:** Go, Java, Kotlin, Scala, C, C++, C#, Ruby, Dart, Lua, Bash and Zig, which have only their made-up repositories.
- **Known limits:** a Rust `mod` line inside a file that is not `lib.rs`, `main.rs` or `mod.rs` looks beside that file, not in its folder. A name imported by name in JavaScript from a file that doesn't define it, such as one it only passes on, can still be guessed elsewhere. Elixir modules used through a short name they were given by `alias … as:` are not followed.
